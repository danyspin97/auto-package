//! A fixup an edit adds to `%prep`, `%build`, `%install` or `%check` needs a comment
//! that says why (convention 40): a shell edit of a file (`sed -i`, `perl -pi`), a
//! `chmod`, `mv`, `ln -s`, an `echo ... >`, a `cp` or `rm` of a file in the source
//! tree or under the build root, an `export` or an env prefix. The comment is the
//! run of `#` lines directly above the fixup, or above the braced `%{python_expand`
//! block the fixup sits in. A fixup the origin spec already has is inherited.

use super::nested_python_expand::brace_depth;
use std::collections::HashMap;

/// The fixup lines of `after` that the edit adds without a comment saying why, each
/// as one message: the line and what to write. Empty when `before` is empty (there
/// is no origin to tell inherited lines from new ones).
pub fn unexplained_fixups(before: &str, after: &str, package: &str) -> Vec<String> {
    if before.trim().is_empty() {
        return Vec::new();
    }
    let lines: Vec<&str> = after.lines().collect();
    let added = added_lines(before, after);
    let mut out = Vec::new();
    let mut section: Option<&str> = None;
    // The opener of the braced `%{python_expand ...}` block the line is in, and the
    // brace depth inside it.
    let mut block: Option<usize> = None;
    let mut depth = 0;
    for (i, &line) in lines.iter().enumerate() {
        if let Some(s) = super::section_word(line) {
            section = Some(s);
            block = None;
            depth = 0;
            continue;
        }
        if !matches!(section, Some("prep" | "build" | "install" | "check")) {
            continue;
        }
        let t = line.trim();
        let mut anchor = i;
        let mut command = t.strip_prefix("%python_expand ").unwrap_or(t);
        if depth > 0 {
            anchor = block.unwrap_or(i);
            depth = brace_depth(line, depth);
            if depth == 0 {
                block = None;
            }
        } else if let Some(rest) = t.strip_prefix("%{python_expand ") {
            command = rest;
            depth = brace_depth(t, 0);
            block = (depth > 0).then_some(i);
        }
        if added[i]
            && is_fixup(command)
            && let Some(problem) = comment_problem(&lines, anchor, package)
        {
            out.push(format!("line {} (`{t}`) {problem}", i + 1));
        }
    }
    out
}

/// Which lines of `after` the edit adds: those whose text `before` does not have
/// as many times, so a repeated line is added once more than the origin had it.
fn added_lines(before: &str, after: &str) -> Vec<bool> {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for l in before.lines() {
        *counts.entry(l.trim()).or_default() += 1;
    }
    after
        .lines()
        .map(|l| match counts.get_mut(l.trim()) {
            Some(c) if *c > 0 => {
                *c -= 1;
                false
            }
            _ => true,
        })
        .collect()
}

/// Whether `command` (a line without its `%python_expand ` prefix) edits a file, a
/// path or the environment of the build.
fn is_fixup(command: &str) -> bool {
    let mut words = command.split_whitespace();
    let Some(first) = words.next() else {
        return false;
    };
    let args: Vec<&str> = words.collect();
    if first == "export" || is_env_assignment(first) {
        return true;
    }
    match first {
        "sed" => args.iter().any(|a| is_sed_in_place(a)),
        "perl" => args.iter().any(|a| is_perl_in_place(a)),
        "chmod" | "mv" => true,
        "ln" => args
            .iter()
            .any(|a| a.starts_with('-') && !a.starts_with("--") && a.contains('s')),
        "rm" => args
            .iter()
            .filter(|a| !a.starts_with('-'))
            .any(|a| is_build_file(a)),
        "cp" => args
            .iter()
            .find(|a| !a.starts_with('-'))
            .is_some_and(|a| is_source_path(a)),
        "echo" => command.contains('>'),
        _ => false,
    }
}

fn is_sed_in_place(arg: &str) -> bool {
    arg == "--in-place"
        || arg.starts_with("--in-place=")
        || (arg.starts_with('-') && !arg.starts_with("--") && arg.contains('i'))
}

/// `perl -i`, `-pi`, `-pi.bak`: the short flags before the `i` are `p`, `n`, `l`, `a`, `w`.
fn is_perl_in_place(arg: &str) -> bool {
    let Some(flags) = arg.strip_prefix('-') else {
        return false;
    };
    !flags.starts_with('-')
        && flags
            .trim_start_matches(['p', 'n', 'l', 'a', 'w'])
            .starts_with('i')
}

/// `PYTHONPATH=...` as the first word of a command: an environment change of the run.
fn is_env_assignment(word: &str) -> bool {
    let Some((name, _)) = word.split_once('=') else {
        return false;
    };
    !name.is_empty()
        && !name.starts_with(|c: char| c.is_ascii_digit())
        && name
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

/// A path under the build root, or a path of the source tree (`rm` of it deletes a
/// file the build made or the sources hold).
fn is_build_file(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.contains("buildroot") || lower.contains("build_root") || is_source_path(path)
}

/// A path in the source tree: relative to the build directory, not absolute, not a
/// macro or variable, and not climbing out of the tree with a `..` component.
fn is_source_path(path: &str) -> bool {
    !path.starts_with(['/', '%', '$']) && !path.split('/').any(|c| c == "..")
}

/// What is missing from the comment above the fixup at `anchor`, `None` when it says
/// why: a `#` run directly above, no macro in it, at least three words of reason
/// that are not only the package name or a TODO.
fn comment_problem(lines: &[&str], anchor: usize, package: &str) -> Option<String> {
    let mut run: Vec<&str> = Vec::new();
    let mut j = anchor;
    while j > 0 && lines[j - 1].trim_start().starts_with('#') {
        run.push(lines[j - 1].trim_start().trim_start_matches('#').trim());
        j -= 1;
    }
    if run.is_empty() {
        return Some(
            "has no comment directly above it that says why: write a `#` line above it (above \
             the `%{python_expand` block when it is in one) with the failing step, the file or \
             the reason, in at least three words and no `%` macro"
                .to_string(),
        );
    }
    run.reverse();
    for c in &run {
        if let Some(m) = macro_in(c) {
            return Some(format!(
                "has a comment above it that names the macro `{m}`: RPM expands a macro in a \
                 comment, so reword the comment without `%`"
            ));
        }
    }
    let text = run.join(" ");
    let lower = text.to_ascii_lowercase();
    let body = ["todo", "fixme", "xxx"]
        .iter()
        .find_map(|p| lower.strip_prefix(p))
        .unwrap_or(lower.as_str());
    let words = body
        .split_whitespace()
        .filter(|w| w.chars().any(char::is_alphanumeric))
        .count();
    let only_package =
        lower.trim_matches(|c: char| !c.is_alphanumeric()) == package.to_ascii_lowercase();
    if words < 3 || only_package {
        return Some(format!(
            "has a comment above it that does not say why (`{text}`): write the failing step, \
             the file or the reason in at least three words"
        ));
    }
    None
}

/// The first macro in a comment (`%{buildroot}`, `%pytest`), `%%` being a literal `%`.
fn macro_in(text: &str) -> Option<String> {
    let s = text.replace("%%", "");
    let mut from = 0;
    while let Some(p) = s[from..].find('%') {
        let at = from + p;
        let next = s[at + 1..].chars().next();
        if next.is_some_and(|c| c.is_alphanumeric() || matches!(c, '{' | '_' | '(' | '!' | '?')) {
            return s[at..].split_whitespace().next().map(str::to_string);
        }
        from = at + 1;
    }
    None
}
