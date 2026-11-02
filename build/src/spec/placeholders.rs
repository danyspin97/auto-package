//! The placeholder check: a `TODO`, `FIXME` or `CHANGEME` the skeleton generators
//! or the agent left in a spec. A name that merely contains the word (an upstream
//! `TODO` file in `%doc`, a `/usr/share/doc/TODO` path) is not a placeholder.

/// Section headers of a spec: a line whose first word is one of these starts a
/// section, and `%files` is the only one whose lines name files.
const SECTIONS: &[&str] = &[
    "%package",
    "%description",
    "%prep",
    "%build",
    "%install",
    "%check",
    "%files",
    "%changelog",
    "%pre",
    "%post",
    "%preun",
    "%postun",
    "%posttrans",
    "%trigger",
    "%filetriggers",
    "%transfiletriggers",
    "%verifyscript",
    "%clean",
    "%sysusers",
    "%generate_buildrequires",
];

/// Tags whose arguments are file names, anywhere in a spec.
const FILE_TAGS: &[&str] = &[
    "%doc", "%license", "%dir", "%attr", "%config", "%ghost", "%exclude", "%defattr", "%verify",
    "%lang",
];

/// Lines of `spec` that still carry a `TODO`, `FIXME` or `CHANGEME` placeholder, as
/// `(line number, text)`. The skeleton generators leave these for the agent to fill
/// in, and a spec that keeps one builds an empty or half-described package. A
/// comment is always searched; a `%files` section and the arguments of `%doc` and
/// `%license` name files, so a file called `TODO` there is not one. The
/// `%changelog` is history, not a placeholder, and is not searched.
pub fn todo_placeholders(spec: &str) -> Vec<(usize, String)> {
    let mut found = Vec::new();
    let mut in_files = false;
    for (i, line) in spec.lines().enumerate() {
        let trimmed = line.trim();
        let head = trimmed.split_whitespace().next().unwrap_or("");
        if head == "%changelog" {
            break;
        }
        if SECTIONS.contains(&head) {
            in_files = head == "%files";
        }
        let names_files = in_files
            || head.starts_with('/')
            || FILE_TAGS
                .iter()
                .any(|t| head == *t || head.starts_with(&format!("{t}(")));
        if (trimmed.starts_with('#') || !names_files) && has_placeholder(trimmed) {
            found.push((i + 1, trimmed.to_string()));
        }
    }
    found
}

/// Whether `text` holds a placeholder marker as a word of its own: not part of a
/// path or file name (`TODO.txt`, `doc/TODO`, `NEWS-TODO`).
fn has_placeholder(text: &str) -> bool {
    const MARKERS: &[&str] = &["TODO", "FIXME", "CHANGEME"];
    let in_name = |c: char| c.is_alphanumeric() || matches!(c, '_' | '/' | '.' | '-');
    MARKERS.iter().any(|m| {
        text.match_indices(m).any(|(at, _)| {
            let before = text[..at].chars().next_back();
            let after = text[at + m.len()..].chars().next();
            !before.is_some_and(in_name) && !after.is_some_and(in_name)
        })
    })
}
