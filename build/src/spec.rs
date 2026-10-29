//! RPM spec generation via py2pack, with post-processing.
//!
//! py2pack handles the openSUSE macro conventions that are otherwise painful
//! to get right. We supply what it can't determine from PyPI alone: the
//! build-backend requirements from the sdist, the correct SPDX license
//! expression, and the upstream URL.

use std::process::{Command, Stdio};

use ap_ecosystem::pypi::{License, Release};
use ap_ecosystem::sdist::BuildSystem;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("could not run py2pack: {0}")]
    Launch(std::io::Error),

    #[error("py2pack exited {code}:\n{stderr}")]
    Failed { code: i32, stderr: String },

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}
mod fixes;
pub use fixes::*;
mod parse;
pub use parse::*;
mod sources;
pub use sources::*;
mod expand;
pub use expand::*;

mod generate;
#[cfg(test)]
mod tests;
pub use generate::*;
mod skeleton;
pub use skeleton::*;
mod rpmlintrc;
pub use rpmlintrc::*;
mod requires_exclude;
pub use requires_exclude::*;
mod wheel;
use wheel::*;

impl HeaderCleanup<'_> {
    /// Whether `%install` removes `path` (relative to site-packages).
    fn removes(&self, path: &str) -> bool {
        match self {
            HeaderCleanup::Dir(d) => path == *d || path.starts_with(&format!("{d}/")),
            HeaderCleanup::Headers(d) => {
                path.starts_with(&format!("{d}/"))
                    && (path.ends_with(".h") || path.ends_with(".hpp"))
            }
        }
    }
}
mod siblings;
pub use siblings::*;

#[cfg(test)]
mod shared_wheel_tests;

/// The four real wheels of NVIDIA's `nvidia-cutlass-dsl-libs-*` 4.7.1 (convention
/// 83): one namespace tree `nvidia_cutlass_dsl/` filled by `-base` (compiled MLIR
/// half, `Root-Is-Purelib: false`), `-core` (pure Python, `true`) and the
/// `-cu12`/`-cu13` flavors (a `.so` each, `false`). The file lists are the
/// wheels' own, copied from the zips.
#[cfg(test)]
mod cutlass_split_wheels_tests;

#[cfg(test)]
mod adopt_sources_tests;
mod license;
pub use license::*;

#[cfg(test)]
mod sdist_license_tests;

#[cfg(test)]
mod prebuilt_library_tests;

#[cfg(test)]
mod generator_tests;

#[cfg(test)]
mod skeleton_header_guard_tests;

#[cfg(test)]
mod expand_order_tests;

#[cfg(test)]
mod review_fix_tests;

/// The architectures `spec`'s top-level `ExclusiveArch:` allows when none of
/// them is `arch`, so a build for `arch` can only end `excluded`. `None` when the
/// spec has no such line, names a macro or `do_not_build` (a switch the tool
/// owns), has it inside a conditional, or allows `arch`.
pub fn exclusive_arch_excluding(spec: &str, arch: &str) -> Option<Vec<String>> {
    let mut depth = 0usize;
    let mut allowed: Vec<String> = Vec::new();
    for line in spec.lines() {
        let t = line.trim_start();
        if t.starts_with("%description") || t.starts_with("%package") {
            break;
        }
        if t.starts_with("%if") {
            depth += 1;
        } else if t.starts_with("%endif") {
            depth = depth.saturating_sub(1);
        } else if depth == 0
            && t.len() > "exclusivearch:".len()
            && t[.."exclusivearch:".len()].eq_ignore_ascii_case("exclusivearch:")
        {
            allowed.extend(
                t["exclusivearch:".len()..]
                    .split_whitespace()
                    .map(str::to_string),
            );
        } else if depth > 0
            && t.get(..14)
                .is_some_and(|h| h.eq_ignore_ascii_case("exclusivearch:"))
        {
            return None;
        }
    }
    if allowed.is_empty()
        || allowed
            .iter()
            .any(|a| a.contains('%') || a == "do_not_build")
    {
        return None;
    }
    let is_x86_32 = |a: &str| matches!(a, "i386" | "i486" | "i586" | "i686");
    let hit = allowed
        .iter()
        .any(|a| a == arch || (is_x86_32(arch) && is_x86_32(a)));
    (!hit).then_some(allowed)
}

/// Lines of `spec` that still carry a `TODO` or `FIXME` placeholder, as `(line number,
/// text)`. The skeleton generators leave these for the agent to fill in, and a
/// spec that keeps one builds an empty or half-described package. The
/// `%changelog` is history, not a placeholder, and is not searched.
pub fn todo_placeholders(spec: &str) -> Vec<(usize, String)> {
    let mut found = Vec::new();
    for (i, line) in spec.lines().enumerate() {
        if line.trim_end() == "%changelog" {
            break;
        }
        if line.contains("TODO") || line.contains("FIXME") {
            found.push((i + 1, line.trim().to_string()));
        }
    }
    found
}

/// Lines of the spec that set a variable whose only purpose is to tell an
/// upstream build "I accept the licence terms" or "this is not a licence
/// violation" (`I_CONFIRM_THIS_IS_NOT_A_LICENSE_VIOLATION=1`,
/// `ACCEPT_EULA=Y`). A guard like that exists because a person has to decide;
/// the review turn set torchcodec's to get its build past a wheel-state check
/// that protects against shipping FFmpeg builds with a different licence. Comments
/// and `%changelog` are not searched.
pub fn license_guard_overrides(spec: &str) -> Vec<(usize, String)> {
    use std::sync::LazyLock;
    static GUARD: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(
            r"(?i)\b[A-Z0-9_]*(?:I_CONFIRM|NOT_A_LICENSE_VIOLATION|LICEN[SC]E_VIOLATION|ACCEPT_[A-Z0-9_]*(?:LICEN[SC]E|EULA|TERMS)|AGREE_[A-Z0-9_]*(?:LICEN[SC]E|EULA|TERMS)|ACCEPT_EULA)[A-Z0-9_]*\s*=",
        )
        .expect("static pattern")
    });
    let mut found = Vec::new();
    for (i, line) in spec.lines().enumerate() {
        if line.trim_end() == "%changelog" {
            break;
        }
        if line.trim_start().starts_with('#') {
            continue;
        }
        if GUARD.is_match(line) {
            found.push((i + 1, line.trim().to_string()));
        }
    }
    found
}

/// The comment a spec carries when a licence-confirmation variable was set
/// after checking what the guard asks about (see
/// [`unverified_license_guards`]).
pub const LICENSE_GUARD_EVIDENCE_TAG: &str = "licence-guard-verified:";

/// The guards of [`license_guard_overrides`] that are not backed by a check.
///
/// A guard that asks the builder to CONFIRM A FACT (`I_CONFIRM_THIS_IS_NOT_A_LICENSE_VIOLATION`:
/// the FFmpeg linked is not GPL-licensed) is satisfied honestly by checking the
/// fact: the spec then carries a `# licence-guard-verified: <what was checked,
/// where>` comment of some substance, and the build goes ahead (a package that
/// cannot build is the worst result; a confirmation nobody checked is the other
/// one). A guard that asks to ACCEPT TERMS (`ACCEPT_EULA`, `AGREE_*_TERMS`) is a
/// person's decision, evidence or not, and is always returned.
pub fn unverified_license_guards(spec: &str) -> Vec<(usize, String)> {
    let evidence = spec
        .lines()
        .take_while(|l| l.trim_end() != "%changelog")
        .filter_map(|l| l.trim_start().strip_prefix('#'))
        .filter_map(|c| {
            let c = c.trim();
            let at = c.to_ascii_lowercase().find(LICENSE_GUARD_EVIDENCE_TAG)?;
            Some(c[at + LICENSE_GUARD_EVIDENCE_TAG.len()..].trim().len())
        })
        .any(|len| len >= 30);
    license_guard_overrides(spec)
        .into_iter()
        .filter(|(_, line)| {
            let upper = line.to_ascii_uppercase();
            let terms = ["EULA", "ACCEPT_", "AGREE_", "TERMS"]
                .iter()
                .any(|t| upper.contains(t));
            terms || !evidence
        })
        .collect()
}

#[cfg(test)]
mod sdist_files_tests;

#[cfg(test)]
mod license_guard_tests;

#[cfg(test)]
mod todo_placeholder_tests;

/// `spec` with its `Version:` set to `version`, keeping everything else: the
/// recipe a version bump carries forward. A `Version:` that is a single macro
/// (`%{package_version}`) is followed to its plain `%define`/`%global` and that
/// is set instead. `None` when the version cannot be found or is computed.
pub fn set_version(spec: &str, version: &str) -> Option<String> {
    let raw = tag_value(spec, "Version:")?;
    let macro_name = raw
        .strip_prefix("%{")
        .and_then(|r| r.strip_suffix('}'))
        .or_else(|| raw.strip_prefix('%'));
    let defines = collect_simple_defines(spec);
    let (prefixes, old_value): (Vec<String>, String) = match macro_name {
        Some(name) if !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_') => {
            (
                vec![format!("%define {name}"), format!("%global {name}")],
                defines.get(name)?.clone(),
            )
        }
        Some(_) => return None,
        None if raw.contains('%') => return None,
        None => (vec!["Version:".to_string()], raw.to_string()),
    };
    let mut replaced = false;
    let lines: Vec<String> = spec
        .lines()
        .map(|line| {
            let t = line.trim_start();
            let matched = if replaced {
                None
            } else {
                prefixes.iter().find(|p| {
                    t.strip_prefix(p.as_str())
                        .is_some_and(|rest| rest.starts_with([' ', '\t']) || rest.is_empty())
                })
            };
            match matched {
                Some(prefix) if old_value.trim().is_empty() => {
                    // No value to replace: the version goes where the value
                    // would start, after the tag's own padding (or one space).
                    replaced = true;
                    let indent = &line[..line.len() - t.len()];
                    let rest = &t[prefix.len()..];
                    let padding = &rest[..rest.len() - rest.trim_start().len()];
                    let padding = if padding.is_empty() { " " } else { padding };
                    format!("{indent}{prefix}{padding}{version}")
                }
                Some(_) if line.contains(old_value.as_str()) => {
                    replaced = true;
                    line.replacen(old_value.as_str(), version, 1)
                }
                _ => line.to_string(),
            }
        })
        .collect();
    replaced.then(|| lines.join("\n") + if spec.ends_with('\n') { "\n" } else { "" })
}

#[cfg(test)]
mod set_version_tests;

#[cfg(test)]
mod normalizer_tests;

/// Collapse the per-file licence lines under one `<name>.dist-info/licenses`
/// directory in `%files` (torch lists ~86 `licenses/third_party/...` files) into
/// the directory itself: `%exclude <dir>` beside `%license <dir>`, so the
/// dist-info entry that also matches them does not list them twice. Applies to a
/// directory with at least three such `%license` lines; the `%exclude` lines for
/// the same files are dropped with them.
pub fn collapse_dist_info_license_lines(spec: &str) -> String {
    const MIN_LINES: usize = 3;
    let dir_of = |line: &str| -> Option<String> {
        let rest = line
            .trim()
            .strip_prefix("%license")
            .or_else(|| line.trim().strip_prefix("%exclude"))?;
        if !rest.starts_with(char::is_whitespace) {
            return None;
        }
        let path = rest.trim();
        let (dir, _file) = path.split_once(".dist-info/licenses/")?;
        (!path.contains(char::is_whitespace)).then(|| format!("{dir}.dist-info/licenses"))
    };
    let mut counts: std::collections::HashMap<String, usize> = Default::default();
    for l in spec.lines().filter(|l| l.trim().starts_with("%license")) {
        if let Some(d) = dir_of(l) {
            *counts.entry(d).or_default() += 1;
        }
    }
    counts.retain(|_, n| *n >= MIN_LINES);
    if counts.is_empty() {
        return spec.to_string();
    }
    let mut done: Vec<String> = Vec::new();
    let mut out: Vec<String> = Vec::new();
    for line in spec.lines() {
        match dir_of(line) {
            Some(d) if counts.contains_key(&d) => {
                if !done.contains(&d) {
                    out.push(format!("%exclude {d}"));
                    out.push(format!("%license {d}"));
                    done.push(d);
                }
            }
            _ => out.push(line.to_string()),
        }
    }
    out.join("\n") + if spec.ends_with('\n') { "\n" } else { "" }
}

#[cfg(test)]
mod license_collapse_tests;

/// The lines of the `%check` section of `spec`, empty when there is none.
fn check_section(spec: &str) -> Vec<&str> {
    let body = spec.split("\n%changelog").next().unwrap_or(spec);
    let mut in_check = false;
    let mut out = Vec::new();
    for line in body.lines() {
        let t = line.trim();
        if t == "%check" {
            in_check = true;
            continue;
        }
        if in_check
            && t.starts_with('%')
            && [
                "%files",
                "%install",
                "%prep",
                "%build",
                "%package",
                "%description",
                "%post",
                "%pre",
                "%preun",
                "%postun",
                "%conf",
                "%generate_buildrequires",
            ]
            .iter()
            .any(|h| t == *h || t.starts_with(&format!("{h} ")))
        {
            break;
        }
        if in_check {
            out.push(line);
        }
    }
    out
}

/// Most `--ignore` options one `%check` may carry before it is a sign that the
/// suite is being cut down for a single root cause (a missing device, a missing
/// fixture directory) instead of replaced by an import smoke test.
pub const MAX_CHECK_IGNORES: usize = 6;

/// `%check` problems a reviewer sends back (convention 67): more than
/// [`MAX_CHECK_IGNORES`] `--ignore` options. Empty when fine.
pub fn check_quality_problems(spec: &str) -> Vec<String> {
    let ignores: usize = check_section(spec)
        .iter()
        .filter(|l| !l.trim_start().starts_with('#'))
        .map(|l| {
            l.split_whitespace()
                .filter(|t| {
                    let t = t.trim_matches(|c| matches!(c, '\'' | '"'));
                    t == "--ignore"
                        || t == "--ignore-glob"
                        || t.starts_with("--ignore=")
                        || t.starts_with("--ignore-glob=")
                })
                .count()
        })
        .sum();
    if ignores > MAX_CHECK_IGNORES {
        return vec![format!(
            "its %check carries {ignores} `--ignore` options: a suite cut down this far has \
             one root cause (a device, a missing fixture directory, a network): find it and \
             select the runnable subset by marker or directory instead of ignoring files one \
             by one (convention 67). An import smoke test only with the quoted log line that \
             proves no subset can run (convention 11, rung iv)"
        )];
    }
    Vec::new()
}

/// Test paths (`tests/test_configs`, `test/data`) the `%check` runs or reads
/// that `exists` says the source does not have. A sdist often leaves the test
/// fixtures out (accelerate's `tests/test_configs`, brotlicffi's `testdata`):
/// the fix is the project's GitHub tag as a second source with the directory
/// extracted in `%prep` (convention 82 b). A path given to `--ignore` or
/// `--deselect` is not read, and one with a `%` or `$` is not a path.
pub fn missing_check_paths(spec: &str, exists: &dyn Fn(&str) -> bool) -> Vec<String> {
    let mut missing: Vec<String> = Vec::new();
    for line in check_section(spec) {
        let t = line.trim();
        if t.starts_with('#') {
            continue;
        }
        let mut skip_next = false;
        for tok in t.split_whitespace() {
            let tok = tok.trim_matches(|c| matches!(c, '\'' | '"'));
            // `--ignore tests/x` (the value in the next word) is not read either.
            if std::mem::take(&mut skip_next) {
                continue;
            }
            if matches!(tok, "--ignore" | "--ignore-glob" | "--deselect") {
                skip_next = true;
                continue;
            }
            let tok = tok.strip_prefix("./").unwrap_or(tok);
            let path = tok.split("::").next().unwrap_or(tok).trim_end_matches('/');
            let under_tests = ["tests/", "test/", "testdata/", "test_data/"]
                .iter()
                .any(|d| path.starts_with(d))
                && path.contains('/');
            let plain = path
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'/'));
            if under_tests && plain && !path.contains("..") && !exists(path) {
                let path = path.to_string();
                if !missing.contains(&path) {
                    missing.push(path);
                }
            }
        }
    }
    missing
}

/// The review problem for [`missing_check_paths`], when there are any.
pub fn missing_check_path_problems(spec: &str, exists: &dyn Fn(&str) -> bool) -> Vec<String> {
    let missing = missing_check_paths(spec, exists);
    // %prep already extracts a directory from a second source.
    if missing.is_empty() || spec.contains("--wildcards") {
        return Vec::new();
    }
    vec![format!(
        "its %check uses {} which the source archive does not ship: add the project's \
         GitHub tag archive as `Source1` (in a `_service` with `download_files`) and extract \
         the directory in %prep after the autosetup with `tar -xzf %{{SOURCE1}} \
         --strip-components=1 --wildcards '*/<dir>/*'` (convention 82 b); or drop the test \
         that needs it",
        missing.join(", ")
    )]
}

#[cfg(test)]
mod missing_check_path_tests;

/// Add the project's tag archive as a further source and extract `dirs` from it
/// in `%prep` (convention 82 b): a `SourceN: <url>#/<name>` line after the last
/// `Source` line, and `tar -xzf %{SOURCEN} --strip-components=1 --wildcards
/// '*/<dir>/*'` after the `%autosetup`/`%setup` of `%prep`. The `download_files`
/// service that fetches it is written by the pipeline when the file is missing.
/// `version` inside `url` becomes `%{version}`. `None` when the spec has no
/// `Source` line or no setup line in `%prep`, already extracts with
/// `--wildcards`, or a directory is not a plain relative path.
pub fn add_fixture_source(spec: &str, url: &str, version: &str, dirs: &[String]) -> Option<String> {
    let plain = |d: &str| {
        !d.is_empty()
            && !d.starts_with('/')
            && !d.contains("..")
            && d.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'/'))
    };
    if dirs.is_empty()
        || !dirs.iter().all(|d| plain(d))
        || spec.contains("--wildcards")
        || url.contains(|c: char| c.is_whitespace() || c == '%' || c == '#')
    {
        return None;
    }
    let lines: Vec<&str> = spec.lines().collect();
    let source_num = |l: &str| -> Option<u32> {
        let rest = l.strip_prefix("Source")?;
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        rest[digits.len()..]
            .trim_start()
            .starts_with(':')
            .then(|| digits.parse().unwrap_or(0))
    };
    let mut last_source = None;
    let mut next = 0;
    for (i, l) in lines.iter().enumerate() {
        if l.starts_with("%description") || l.starts_with("%package") {
            break;
        }
        if let Some(n) = source_num(l) {
            last_source = Some(i);
            next = next.max(n + 1);
        }
    }
    let last_source = last_source?;
    let prep = lines.iter().position(|l| l.trim() == "%prep")?;
    let mut setup = None;
    for (i, l) in lines.iter().enumerate().skip(prep + 1) {
        let t = l.trim();
        if t.starts_with('%')
            && !t.starts_with("%autosetup")
            && !t.starts_with("%setup")
            && !t.starts_with("%{")
            && !t.starts_with("%if")
            && !t.starts_with("%endif")
            && !t.starts_with("%else")
        {
            break;
        }
        if t.starts_with("%autosetup") || t.starts_with("%setup") {
            // The directive may continue on the next lines.
            let mut end = i;
            while lines[end].trim_end().ends_with('\\') && end + 1 < lines.len() {
                end += 1;
            }
            setup = Some(end);
            break;
        }
    }
    let setup = setup?;
    let url = if version.is_empty() {
        url.to_string()
    } else {
        url.replace(version, "%{version}")
    };
    let file = url.rsplit('/').next().unwrap_or("fixtures.tar.gz");
    let repo = url
        .split('/')
        .nth(4)
        .filter(|r| !r.is_empty())
        .unwrap_or("fixtures");
    let rename = format!("{repo}-fixtures-{file}");
    let wild: Vec<String> = dirs
        .iter()
        .map(|d| format!("'*/{}/*'", d.trim_end_matches('/')))
        .collect();
    let tar = format!(
        "tar -xzf %{{SOURCE{next}}} --strip-components=1 --wildcards {}",
        wild.join(" ")
    );
    let mut out: Vec<String> = Vec::with_capacity(lines.len() + 3);
    for (i, l) in lines.iter().enumerate() {
        out.push((*l).to_string());
        if i == last_source {
            out.push(format!("Source{next}:        {url}#/{rename}"));
        }
        if i == setup {
            out.push(format!("# The sdist leaves out {}", dirs.join(", ")));
            out.push(tar.clone());
        }
    }
    let mut text = out.join("\n");
    if spec.ends_with('\n') {
        text.push('\n');
    }
    Some(text)
}

#[cfg(test)]
mod add_fixture_source_tests;

/// [`add_fixture_source`] for the paths [`missing_check_paths`] reports: the
/// repository comes from the sdist metadata (`pkg_info`, `pyproject`), the tag
/// archive URL from `resolve(repo, version)`. `None` when there is nothing
/// missing, no repository, or no tag, so the reviewer prompt stays the fallback.
pub fn auto_fixture_source(
    spec: &str,
    exists: &dyn Fn(&str) -> bool,
    pkg_info: &str,
    pyproject: &str,
    version: &str,
    resolve: &dyn Fn(&str, &str) -> Option<String>,
) -> Option<String> {
    let missing = missing_check_paths(spec, exists);
    if missing.is_empty() || spec.contains("--wildcards") {
        return None;
    }
    // One directory at the top of tests is enough when the missing path is a
    // file inside a fixtures directory already listed.
    let mut dirs: Vec<String> = Vec::new();
    for m in missing {
        if !dirs
            .iter()
            .any(|d| m == *d || m.starts_with(&format!("{d}/")))
        {
            dirs.retain(|d| !d.starts_with(&format!("{m}/")));
            dirs.push(m);
        }
    }
    let repo = ap_ecosystem::sdist::source_repo_from_metadata(pkg_info, pyproject)?;
    let url = resolve(&repo, version)?;
    add_fixture_source(spec, &url, version, &dirs)
}

#[cfg(test)]
mod auto_fixture_source_tests;

#[cfg(test)]
mod sharp_edge_tests;

/// Drop the `python-pytest` BuildRequires of a spec whose `%check` never runs
/// pytest (an import smoke test): the dependency only slows the build and
/// pulls plugins in. A spec with no `%check` section, or one that mentions
/// pytest, is left alone.
pub fn drop_unused_pytest_build_requires(spec: &str) -> String {
    let check = check_section(spec);
    if check.is_empty()
        || check
            .iter()
            .any(|l| !l.trim_start().starts_with('#') && l.contains("pytest"))
    {
        return spec.to_string();
    }
    let mut out: Vec<&str> = Vec::new();
    for line in spec.lines() {
        let t = line.trim();
        let is_pytest = t.starts_with("BuildRequires:")
            && t.trim_start_matches("BuildRequires:")
                .split_whitespace()
                .eq(["%{python_module", "pytest}"]);
        if !is_pytest {
            out.push(line);
        }
    }
    out.join("\n") + if spec.ends_with('\n') { "\n" } else { "" }
}

#[cfg(test)]
mod check_quality_tests;

/// Whether the spec's `%check` does nothing but test that a file exists
/// (`test -e`, `test -x`, `ls`, also behind `%python_expand`): it has at least
/// one such command and no other. A package that ships Python modules is
/// imported instead (convention 67).
pub fn check_is_file_probe_only(spec: &str) -> bool {
    let body = spec.split("\n%changelog").next().unwrap_or(spec);
    let mut in_check = false;
    let mut probes = 0;
    for line in body.lines() {
        let t = line.trim();
        if t == "%check" {
            in_check = true;
            continue;
        }
        const SECTIONS: &[&str] = &[
            "%files",
            "%changelog",
            "%install",
            "%prep",
            "%build",
            "%package",
            "%description",
            "%post",
            "%pre",
            "%preun",
            "%postun",
            "%conf",
            "%generate_buildrequires",
        ];
        if in_check
            && SECTIONS
                .iter()
                .any(|h| t == *h || t.starts_with(&format!("{h} ")))
        {
            break;
        }
        if !in_check || t.is_empty() || t.starts_with('#') {
            continue;
        }
        let command = t.strip_prefix("%python_expand").map(str::trim).unwrap_or(t);
        if command.starts_with("test -e")
            || command.starts_with("test -x")
            || command.starts_with("test -f")
            || command.starts_with("ls ")
        {
            probes += 1;
        } else if t.starts_with("%if") || t.starts_with("%endif") || t.starts_with("%else") {
            continue;
        } else {
            return false;
        }
    }
    probes > 0
}

#[cfg(test)]
mod console_script_tests;

#[cfg(test)]
mod check_probe_tests;

/// Parse `op version` (`>= 0.1.10`, `< 0.1.12`, `= 2.0`) of one RPM dependency
/// item and say how `shipped` stands against it: `Some(None)` when it meets it,
/// `Some(Some(true))` when it is too new, `Some(Some(false))` when too old.
/// `None` when it is not a constraint this reads.
pub(crate) fn rpm_constraint_miss(
    constraint: &str,
    shipped: &pep440_rs::Version,
) -> Option<Option<bool>> {
    use std::str::FromStr;
    let c = constraint.trim();
    let (op, rest) = ["<=", ">=", "==", "<", ">", "="]
        .iter()
        .find_map(|op| c.strip_prefix(op).map(|r| (*op, r.trim())))?;
    // An RPM version may carry an epoch or a release: only the upstream part.
    let ver = rest.split(['-', ' ']).next().unwrap_or(rest);
    let ver = ver.rsplit_once(':').map(|(_, v)| v).unwrap_or(ver);
    let wanted = pep440_rs::Version::from_str(ver).ok()?;
    let met = match op {
        "<=" => shipped <= &wanted,
        ">=" => shipped >= &wanted,
        "<" => shipped < &wanted,
        ">" => shipped > &wanted,
        _ => shipped == &wanted,
    };
    if met {
        return Some(None);
    }
    Some(Some(match op {
        "<=" | "<" => true,
        ">=" | ">" => false,
        _ => shipped > &wanted,
    }))
}

/// The dependency lines of `spec` for `dependency` with every version
/// constraint the version `shipped` misses removed, the name kept: a package the
/// distro ships is used as it is, and the bound that excludes it (an upstream
/// `<= 0.1.11`, an exact `==` pin) is dropped instead of the dependency
/// (convention 59). Constraints it meets stay. Duplicate lines this leaves are
/// merged.
pub fn relax_dependency_bounds(spec: &str, dependency: &str, shipped: &str) -> String {
    use std::str::FromStr;
    let Ok(shipped_v) = pep440_rs::Version::from_str(shipped.trim()) else {
        return spec.to_string();
    };
    let wanted = crate::osc::fuzzy_key(dependency);
    let mut out: Vec<String> = Vec::new();
    // One dedupe set per conditional branch: the same text in the `%if` and the
    // `%else` branch is two different requirements, not a duplicate.
    let mut seen_stack: Vec<std::collections::HashSet<String>> = vec![Default::default()];
    const OPEN_CONDITIONALS: &[&str] = &["%if", "%ifarch", "%ifnarch", "%ifos", "%ifnos"];
    const BRANCH_CONDITIONALS: &[&str] = &[
        "%elif",
        "%elifarch",
        "%elifnarch",
        "%elifos",
        "%elifnos",
        "%else",
    ];
    for line in spec.lines() {
        let t = line.trim_start();
        let keyword = t.split_whitespace().next().unwrap_or_default();
        if OPEN_CONDITIONALS.contains(&keyword) {
            seen_stack.push(Default::default());
            out.push(line.to_string());
            continue;
        }
        if BRANCH_CONDITIONALS.contains(&keyword) {
            if let Some(top) = seen_stack.last_mut() {
                top.clear();
            }
            out.push(line.to_string());
            continue;
        }
        if keyword == "%endif" {
            if seen_stack.len() > 1 {
                seen_stack.pop();
            }
            out.push(line.to_string());
            continue;
        }
        let is_dep_line = ["BuildRequires", "Requires", "Recommends", "Suggests"]
            .iter()
            .any(|tag| {
                t.strip_prefix(tag)
                    .is_some_and(|r| r.starts_with(':') || r.starts_with('('))
            });
        if !is_dep_line {
            out.push(line.to_string());
            continue;
        }
        let (head, value) = line.split_once(':').unwrap_or((line, ""));
        let items: Vec<String> = value
            .split(',')
            .map(|item| {
                let item = item.trim();
                // `%{python_module name >= v}` or `python-name >= v`.
                let (open, inner, close) = match item
                    .strip_prefix("%{python_module ")
                    .and_then(|r| r.strip_suffix('}'))
                {
                    Some(inner) => ("%{python_module ", inner, "}"),
                    None => ("", item, ""),
                };
                let name_end = inner
                    .find(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | '='))
                    .unwrap_or(inner.len());
                let name = &inner[..name_end];
                let key = crate::osc::fuzzy_key(
                    name.strip_prefix("python-")
                        .or_else(|| name.strip_prefix("python3-"))
                        .unwrap_or(name),
                );
                let key_plain = crate::osc::fuzzy_key(name);
                if key != wanted && key_plain != wanted {
                    return item.to_string();
                }
                let constraint = inner[name_end..].trim();
                if constraint.is_empty() || constraint.contains(" if ") {
                    return item.to_string();
                }
                // `%{python_module trio >= 0.25.1 with %python-trio < 0.34}`:
                // each clause is judged on its own, the ones met stay.
                if !open.is_empty() && constraint.contains(" with ") {
                    let clauses: Vec<&str> = constraint.split(" with ").collect();
                    let kept: Vec<&str> = clauses
                        .iter()
                        .map(|c| c.trim_start_matches(|ch: char| !matches!(ch, '<' | '>' | '=')))
                        .map(str::trim)
                        .filter(|c| !matches!(rpm_constraint_miss(c, &shipped_v), Some(Some(_))))
                        .collect();
                    return if kept.len() == clauses.len() {
                        item.to_string()
                    } else if kept.is_empty() {
                        format!("{open}{name}{close}")
                    } else {
                        format!(
                            "{open}{name} {}{close}",
                            kept.join(&format!(" with %python-{name} "))
                        )
                    };
                }
                match rpm_constraint_miss(constraint, &shipped_v) {
                    Some(Some(_)) => format!("{open}{name}{close}"),
                    _ => item.to_string(),
                }
            })
            .collect();
        let rebuilt = format!(
            "{head}:{}{}",
            if value.starts_with(' ') || value.starts_with('\t') {
                &value[..value.len() - value.trim_start().len()]
            } else {
                " "
            },
            items.join(", ")
        );
        // A line that reads the same as one already kept in this branch is not
        // repeated.
        let seen = seen_stack
            .last_mut()
            .expect("the base scope is never popped");
        if rebuilt != line && !seen.insert(rebuilt.trim().to_string()) {
            continue;
        }
        seen.insert(rebuilt.trim().to_string());
        out.push(rebuilt);
    }
    out.join("\n") + if spec.ends_with('\n') { "\n" } else { "" }
}

/// [`relax_dependency_bounds`] for each `(dependency, shipped version)` a
/// package's state records (`PkgState::relaxed_dependencies`): applied on every
/// validation, root package included, before anything is built.
pub fn relax_dependencies(spec: &str, relaxed: &[(String, String)]) -> String {
    relaxed
        .iter()
        .fold(spec.to_string(), |spec, (dep, shipped)| {
            relax_dependency_bounds(&spec, dep, shipped)
        })
}

#[cfg(test)]
mod relax_bound_tests;

/// Whether `requirement` is a package a spec can name: `%{python_module ...}`
/// around it is allowed, and so is a version constraint after the name. The
/// name must match `^[A-Za-z0-9][A-Za-z0-9._+-]*$`; the constraint may hold only
/// letters, digits, spaces and `. _ + - < > = ! ~ , *`, so no macro, shell
/// or spec character reaches the line.
fn is_plain_build_requirement(requirement: &str) -> bool {
    let inner = requirement
        .strip_prefix("%{python_module ")
        .and_then(|r| r.strip_suffix('}'))
        .unwrap_or(requirement)
        .trim();
    let name_end = inner
        .find(|c: char| !(c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | '-')))
        .unwrap_or(inner.len());
    let (name, constraint) = inner.split_at(name_end);
    name.starts_with(|c: char| c.is_ascii_alphanumeric())
        && constraint
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || " ._+-<>=!~,*".contains(c))
}

/// `spec` with `BuildRequires:  <requirement>` added after the last
/// `BuildRequires:` line, or `None` when it is already there, or the name is
/// not plain (a warning is logged). Used to settle a provider the solver cannot
/// choose between.
pub fn add_build_requires(spec: &str, requirement: &str) -> Option<String> {
    let wanted = requirement.trim();
    if !is_plain_build_requirement(wanted) {
        tracing::warn!("not adding BuildRequires {wanted:?}: not a plain package name");
        return None;
    }
    let has = spec.lines().any(|l| {
        l.trim_start()
            .strip_prefix("BuildRequires:")
            .is_some_and(|v| v.trim() == wanted)
    });
    if has {
        return None;
    }
    let lines: Vec<&str> = spec.lines().collect();
    let at = lines
        .iter()
        .rposition(|l| l.trim_start().starts_with("BuildRequires:"))?;
    let mut out: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
    out.insert(at + 1, format!("BuildRequires:  {wanted}"));
    Some(out.join("\n") + if spec.ends_with('\n') { "\n" } else { "" })
}

#[cfg(test)]
mod runtime_dependency_tests;

#[cfg(test)]
mod add_build_requires_tests;

#[cfg(test)]
mod conditional_dependency_tests;

#[cfg(test)]
mod hashed_libs_exclude_tests;

#[cfg(test)]
mod redist_requires_tests;

#[cfg(test)]
mod wheel_generator_tests;

#[cfg(test)]
mod license_url_injection_tests;

#[cfg(test)]
mod exclusive_arch_tests;
