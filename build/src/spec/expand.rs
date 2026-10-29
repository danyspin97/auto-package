//! Expanding the simple macros of a spec: the defines, the conditional frames and
//! the autosetup directory.

/// Collect `%define <name> <value>` and `%global <name> <value>` entries
/// from a spec, in file order so later definitions override earlier ones (as
/// rpmbuild itself does). Only the simple single-token-value shape is
/// understood — parametric macros, conditionals, nested `%()` evaluation,
/// and the lua `%{lua:...}` form are intentionally left untouched; this is
/// a header reader, not an rpm interpreter.
pub(super) fn collect_simple_defines(spec: &str) -> std::collections::HashMap<String, String> {
    let mut macros = std::collections::HashMap::new();
    // One frame per open `%if`. A define is only recorded when every frame's
    // current branch is known to be taken: recording from an unevaluable
    // branch could silently pick the wrong value.
    let mut stack: Vec<CondFrame> = Vec::new();
    for line in spec.lines() {
        let trimmed = line.trim_start();
        if let Some(cond) = trimmed
            .strip_prefix("%if ")
            .or_else(|| trimmed.strip_prefix("%if\t"))
        {
            let state = eval_condition(cond, &macros);
            stack.push(CondFrame::new(state));
            continue;
        }
        if trimmed.starts_with("%ifarch")
            || trimmed.starts_with("%ifnarch")
            || trimmed.starts_with("%ifos")
            || trimmed.starts_with("%ifnos")
        {
            stack.push(CondFrame::new(None));
            continue;
        }
        if let Some(cond) = trimmed.strip_prefix("%elif ") {
            if let Some(frame) = stack.last_mut() {
                frame.advance();
                frame.state = match frame.prior {
                    Taken::No => eval_condition(cond, &macros),
                    _ => None,
                };
            }
            continue;
        }
        if trimmed == "%else" {
            if let Some(frame) = stack.last_mut() {
                frame.advance();
                frame.state = match frame.prior {
                    Taken::Yes => Some(false),
                    Taken::Maybe => None,
                    Taken::No => Some(true),
                };
            }
            continue;
        }
        if trimmed == "%endif" {
            stack.pop();
            continue;
        }
        if stack.iter().any(|f| f.state != Some(true)) {
            continue;
        }
        let rest = trimmed
            .strip_prefix("%define ")
            .or_else(|| trimmed.strip_prefix("%global "));
        let Some(rest) = rest else { continue };
        let rest = rest.trim();
        let Some((name, value)) = rest.split_once(char::is_whitespace) else {
            continue;
        };
        let name = name.trim();
        let value = value.trim();
        // Skip parametric macros (name(args)) and empty definitions.
        if name.is_empty() || value.is_empty() || name.contains('(') {
            continue;
        }
        // One-pass substitution of already-known defines so chained
        // `%define release_prefix 1.0` / `%define pkgver %{release_prefix}.3`
        // resolves. Deliberately not recursive beyond that — complex nested
        // forms belong to real rpm expansion, not this reader.
        macros.insert(name.to_string(), expand_once(value, &macros));
    }
    macros
}

/// Whether an earlier branch of the current `%if` chain was taken.
#[derive(Clone, Copy, PartialEq)]
enum Taken {
    Yes,
    No,
    Maybe,
}

struct CondFrame {
    /// `Some(true)` taken, `Some(false)` skipped, `None` cannot be evaluated.
    state: Option<bool>,
    prior: Taken,
}

impl CondFrame {
    pub(super) fn new(state: Option<bool>) -> Self {
        CondFrame {
            state,
            prior: Taken::No,
        }
    }

    /// Fold the branch just left into `prior` before moving to the next one.
    fn advance(&mut self) {
        self.prior = match (self.prior, self.state) {
            (Taken::Yes, _) | (_, Some(true)) => Taken::Yes,
            (Taken::Maybe, _) | (_, None) => Taken::Maybe,
            _ => Taken::No,
        };
    }
}

/// Evaluate the simple `%if` shapes: an integer literal, optionally with a
/// `%{?name}` expansion (`0%{?name}`). A macro the spec itself never defined
/// is unknown, not false — rpm's own config may define it.
fn eval_condition(cond: &str, macros: &std::collections::HashMap<String, String>) -> Option<bool> {
    let cond = cond.trim();
    let mut expanded = String::new();
    let mut rest = cond;
    while let Some(pos) = rest.find("%{?") {
        expanded.push_str(&rest[..pos]);
        let after = &rest[pos + 3..];
        let end = after.find('}')?;
        let name = &after[..end];
        if name.is_empty() || !name.chars().all(|c| c.is_alphanumeric() || c == '_') {
            return None;
        }
        expanded.push_str(macros.get(name)?);
        rest = &after[end + 1..];
    }
    expanded.push_str(rest);
    let n: i64 = expanded.trim().parse().ok()?;
    Some(n != 0)
}

/// Replace every `%{name}` or `%name` reference by its collected `%define`/
/// `%global` value. Unknown macros are left alone so the caller still sees
/// which ones didn't resolve.
pub(crate) fn expand_simple_defines(
    s: &str,
    macros: &std::collections::HashMap<String, String>,
) -> String {
    expand_once(s, macros)
}

/// One left-to-right scan that replaces only whole macro identifiers
/// (`%{ident}` and `%ident`, ident = `[A-Za-z0-9_]+`). `%%`, `%{?x}`,
/// `%{!?x}`, `%(...)` and unknown names are copied through untouched.
pub(super) fn expand_once(s: &str, macros: &std::collections::HashMap<String, String>) -> String {
    let is_ident = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(pos) = rest.find('%') {
        out.push_str(&rest[..pos]);
        let after = &rest[pos + 1..];
        let (consumed, value) = if after.starts_with('%') {
            (1, None)
        } else if let Some(inner) = after.strip_prefix('{') {
            match inner.find('}') {
                Some(end) if !inner[..end].is_empty() && inner[..end].chars().all(is_ident) => {
                    (end + 2, macros.get(&inner[..end]))
                }
                _ => (0, None),
            }
        } else if after.starts_with('(') {
            let mut depth = 0usize;
            let mut end = after.len();
            for (i, c) in after.char_indices() {
                match c {
                    '(' => depth += 1,
                    ')' => {
                        depth -= 1;
                        if depth == 0 {
                            end = i + 1;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            (end, None)
        } else {
            let len = after.find(|c: char| !is_ident(c)).unwrap_or(after.len());
            (len, macros.get(&after[..len]).filter(|_| len > 0))
        };
        match value {
            Some(v) => out.push_str(v),
            None => {
                out.push('%');
                out.push_str(&after[..consumed]);
            }
        }
        rest = &after[consumed..];
    }
    out.push_str(rest);
    out
}

/// Replace `version` in a directory name with `%{version}`, but only where it
/// is not embedded in a longer alphanumeric run. A plain substring replace
/// turned `sqlite3-3` with version `3` into `sqlite%{version}-%{version}`.
///
/// After the version a letter or digit blocks the match (`1.2.33`), while `.`
/// or `-` allow it (`foo-1.2.3.post1`). Before it, a letter is tolerated only
/// for a dotted version (`foo1.2.3`, `foo-v1.2.3`): a bare `3` right after
/// letters is as likely part of a name as a version. A digit never precedes.
pub(super) fn replace_version_component(dir: &str, version: &str) -> String {
    if version.is_empty() {
        return dir.to_string();
    }
    let bytes = dir.as_bytes();
    let dotted = version.contains('.');
    let mut out = String::with_capacity(dir.len());
    let mut i = 0;
    while i < dir.len() {
        let end = i + version.len();
        let at_start = i == 0
            || !bytes[i - 1].is_ascii_alphanumeric()
            || (dotted && bytes[i - 1].is_ascii_alphabetic());
        let at_end = end >= dir.len() || !bytes[end].is_ascii_alphanumeric();
        if dir[i..].starts_with(version) && at_start && at_end {
            out.push_str("%{version}");
            i = end;
        } else {
            let ch = dir[i..].chars().next().unwrap();
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    out
}

/// Whether a directory name may be written unescaped into a `%autosetup -n`
/// line. Same rule as the sdist unpacker (`ap_ecosystem::sdist`), so a name
/// accepted there is accepted here: no whitespace or control characters, no
/// `%` macro, no `/` or shell metacharacters.
pub(super) fn is_safe_autosetup_dir(dir: &str) -> bool {
    ap_ecosystem::sdist::is_safe_top_level_dir(dir)
}

/// The expanded `%autosetup -n` directory for a spec with the given name and
/// version. Used by `check_autosetup_dir` and exposed for testing.
pub fn autosetup_expected_dir(spec: &str, name: &str, version: &str) -> Option<String> {
    let raw = spec
        .lines()
        .find(|l| l.trim().starts_with("%autosetup"))?
        .split_whitespace()
        .collect::<Vec<_>>()
        .windows(2)
        .find(|w| w[0] == "-n")
        .map(|w| w[1].to_string())
        .unwrap_or_else(|| format!("{name}-%{{version}}"));

    Some(
        raw.replace("%{name}", name)
            .replace("%{version}", version)
            .replace("%{upstream_version}", version),
    )
}

/// Strip all `# ap: fix` comment lines from a spec.
///
/// Model-authored fix comments must not accumulate across attempts: they
/// reference RPM macros (e.g. `# ap: fix 1: Changed %pytest to %pytest_arch`)
/// and RPM expands macros inside comments, causing parse failures. Call this
/// before handing the spec back to the model.
pub fn strip_ap_fix_comments(spec: &str) -> String {
    let filtered: Vec<&str> = spec
        .lines()
        .filter(|l| !l.trim_start().starts_with("# ap: fix"))
        .collect();
    let body = filtered.join("\n");
    if spec.ends_with('\n') {
        body + "\n"
    } else {
        body
    }
}

/// Return the 1-based line number and text of the first comment line that
/// contains a `%` macro reference, or `None` if the spec is clean.
///
/// RPM expands macros inside comment lines, so any `# foo: %something` line
/// will be expanded before the spec is parsed — causing the mysterious
/// `Unknown tag: set +x` class of failure that item 8 exists to prevent.
pub fn first_macro_in_comment(spec: &str) -> Option<(usize, String)> {
    for (i, line) in spec.lines().enumerate() {
        let trimmed = line.trim_start();
        if !trimmed.starts_with('#') {
            continue;
        }
        if let Some(pos) = trimmed.find('%') {
            let after = &trimmed[pos + 1..];
            // `%%` is a literal percent — not a macro.
            if after.starts_with('%') {
                continue;
            }
            if after.starts_with(|c: char| c.is_alphabetic() || c == '{') {
                return Some((i + 1, line.to_string()));
            }
        }
    }
    None
}

/// Insert `cd <dir>` at the top of `%build` and `%install`.
///
/// A repository snapshot of a monorepo has the packaging in a subdirectory
/// (`cuda_python/`, `python/`), so `%autosetup` lands one level above the
/// project and every build macro would run in the wrong place.
pub fn build_in_subdir(spec: &str, dir: &str) -> String {
    // The directory is written into `%build`/`%install`: an unsafe one leaves
    // the spec as it is rather than running a `cd` built from archive text.
    if !ap_ecosystem::sdist::is_safe_manifest_dir(dir) || dir.is_empty() {
        return spec.to_string();
    }
    let mut out = String::with_capacity(spec.len() + 64);
    for line in spec.lines() {
        out.push_str(line);
        out.push('\n');
        if matches!(line.trim(), "%build" | "%install") {
            out.push_str(&format!("cd {dir}\n"));
        }
    }
    if !spec.ends_with('\n') {
        out.pop();
    }
    out
}
