//! Directories a spec's `%files` lists files in but never owns.
//!
//! OBS fails a build in `50-check-filelist` when a package creates a
//! directory that no package owns: `%files` lists `%{_datadir}/x/y.conf` and
//! no `%dir %{_datadir}/x`. The check is static: the `%files` sections say
//! which paths the package lists and which directories it owns, and a
//! whitelist ([`super::standard_dirs`]) says which directories every system
//! owns already. `%install` is not read: what it creates matters only when
//! `%files` lists it, and a command such as `%meson_install` cannot be
//! predicted from the spec.
//!
//! What counts as an owner: a `%dir` entry; an entry with a trailing slash;
//! an entry that is the parent of another listed path (a directory entry
//! owns its tree); a glob that matches the directory or one above it; a
//! `%doc` or `%license` line in the same section owns the package's
//! `%{_docdir}` and `/usr/share/licenses` directories.
//! A line ending in a backslash is joined with the next one.
//! Every `%files` section and every `%if` branch is read, so ownership
//! anywhere counts. What bails out: `%files -f <list>` skips the whole spec
//! (the list may own directories); an entry with a macro this module does
//! not know, a `$variable` or a quote is skipped.

use super::standard_dirs::is_standard_dir;
use super::{first_word, is_section};
use std::collections::BTreeSet;

/// A directory that needs a `%dir` line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissingDir {
    /// The 0-based number of the first `%files` line that lists a path in
    /// the directory.
    pub line: usize,
    /// The directory as that line writes it, macros unexpanded
    /// (`%{_datadir}/x`).
    pub text: String,
    /// The directory with the macros expanded (`/usr/share/x`).
    pub path: String,
}

/// The macros of the `%files` paths this module expands.
const MACROS: &[(&str, &str)] = &[
    ("_prefix", "/usr"),
    ("_exec_prefix", "/usr"),
    ("_bindir", "/usr/bin"),
    ("_sbindir", "/usr/sbin"),
    ("_datadir", "/usr/share"),
    ("_datarootdir", "/usr/share"),
    ("_sysconfdir", "/etc"),
    ("_libexecdir", "/usr/libexec"),
    ("_mandir", "/usr/share/man"),
    ("_infodir", "/usr/share/info"),
    ("_docdir", "/usr/share/doc/packages"),
    ("_includedir", "/usr/include"),
    ("_localstatedir", "/var"),
    ("_sharedstatedir", "/var/lib"),
    ("_unitdir", "/usr/lib/systemd/system"),
    ("_userunitdir", "/usr/lib/systemd/user"),
    ("_sysusersdir", "/usr/lib/sysusers.d"),
    ("_tmpfilesdir", "/usr/lib/tmpfiles.d"),
    ("_udevrulesdir", "/usr/lib/udev/rules.d"),
    ("_udevdir", "/usr/lib/udev"),
    ("_libdir", "/usr/lib64"),
];

/// The `Name` and `Version` of the preamble, `None` when it is not a plain
/// value.
pub(crate) struct Facts {
    name: Option<String>,
    version: Option<String>,
}

pub(crate) fn facts(lines: &[&str]) -> Facts {
    let mut facts = Facts {
        name: None,
        version: None,
    };
    for line in lines.iter().take_while(|l| !is_section(l)) {
        let Some((tag, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        if value.is_empty() || value.contains(['%', '$']) {
            continue;
        }
        match tag.trim().to_ascii_lowercase().as_str() {
            "name" if facts.name.is_none() => facts.name = Some(value.to_string()),
            "version" if facts.version.is_none() => facts.version = Some(value.to_string()),
            _ => {}
        }
    }
    facts
}

/// `text` with its macros expanded; `None` when one is not known.
pub(crate) fn expand(text: &str, facts: &Facts) -> Option<String> {
    if text.contains(['$', '"', '\'']) {
        return None;
    }
    let mut out = String::new();
    let mut rest = text;
    while let Some(at) = rest.find('%') {
        out.push_str(&rest[..at]);
        rest = &rest[at + 1..];
        let name = if let Some(body) = rest.strip_prefix('{') {
            let end = body.find('}')?;
            rest = &body[end + 1..];
            &body[..end]
        } else {
            let end = rest
                .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .unwrap_or(rest.len());
            let name = &rest[..end];
            rest = &rest[end..];
            name
        };
        match name {
            "name" => out.push_str(facts.name.as_deref()?),
            "version" => out.push_str(facts.version.as_deref()?),
            _ => out.push_str(MACROS.iter().find(|(k, _)| *k == name)?.1),
        }
    }
    out.push_str(rest);
    Some(out)
}

/// One `%files` section: the package it builds, and whether it lists `%doc`
/// or `%license` files. Those files own the package's doc and license
/// directories, so the section needs no `%dir` line for them.
pub(crate) struct Section {
    package: Option<String>,
    docs: bool,
    /// The 0-based number of the last entry line of the section outside any conditional, its
    /// `%files` header when it has no other.
    pub(crate) last: usize,
}

/// The package a `%files` header builds: the main package, `-n <name>` or
/// `<main>-<sub>`. `None` when it cannot be told.
fn section_package(header: &str, facts: &Facts) -> Option<String> {
    let mut words = header.split_whitespace().skip(1);
    match words.next() {
        None => facts.name.clone(),
        Some("-n") => expand(words.next()?, facts),
        Some(sub) if !sub.starts_with('-') => Some(format!("{}-{sub}", facts.name.as_deref()?)),
        Some(_) => None,
    }
}

/// Whether `section` owns `dir` through its `%doc` or `%license` files.
fn implicit_owner(section: &Section, dir: &str) -> bool {
    match section.package.as_deref() {
        Some(pkg) if section.docs => {
            dir == format!("/usr/share/doc/packages/{pkg}")
                || dir == format!("/usr/share/licenses/{pkg}")
        }
        _ => false,
    }
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Kind {
    Dir,
    File,
    Tree,
    Glob,
}

pub(crate) struct Entry {
    pub(crate) line: usize,
    pub(crate) text: String,
    pub(crate) path: String,
    pub(crate) kind: Kind,
    pub(crate) section: usize,
}

/// The `%files` entries of one line: the directives before the path
/// (`%attr(...)`, `%config(...)`, `%doc`, ...) are stripped. `section` is
/// the index of the line's section in `sections`, which records `%doc` and
/// `%license` lines.
fn entries_of(
    line: &str,
    number: usize,
    section: usize,
    facts: &Facts,
    sections: &mut [Section],
) -> Vec<Entry> {
    const DIRECTIVES: &[&str] = &[
        "attr",
        "config",
        "verify",
        "lang",
        "caps",
        "doc",
        "license",
        "ghost",
        "dir",
        "docdir",
        "readme",
        "artifact",
        "missingok",
        "exclude",
        "defattr",
    ];
    let mut rest = line.trim();
    let mut seen: Vec<&str> = Vec::new();
    while let Some(after) = rest.strip_prefix('%') {
        let end = after
            .find(|c: char| !c.is_ascii_alphanumeric())
            .unwrap_or(after.len());
        let name = &after[..end];
        if !DIRECTIVES.contains(&name) {
            break;
        }
        seen.push(name);
        rest = &after[end..];
        if rest.starts_with('(') {
            let Some(close) = rest.find(')') else {
                return Vec::new();
            };
            rest = &rest[close + 1..];
        }
        rest = rest.trim_start();
    }
    let docs = seen.contains(&"doc") || seen.contains(&"license");
    if docs {
        sections[section].docs = true;
    }
    if ["exclude", "defattr", "docdir"]
        .iter()
        .any(|d| seen.contains(d))
    {
        return Vec::new();
    }
    let relative_ok = !docs;
    let mut out = Vec::new();
    for word in rest.split_whitespace() {
        if word.starts_with('#') {
            break;
        }
        if !relative_ok && !word.starts_with(['/', '%']) {
            continue;
        }
        let Some(path) = expand(word, facts).filter(|p| p.starts_with('/')) else {
            continue;
        };
        let (kind, text, path) = if seen.contains(&"dir") {
            (
                Kind::Dir,
                word.trim_end_matches('/'),
                path.trim_end_matches('/'),
            )
        } else if path.ends_with('/') {
            (
                Kind::Tree,
                word.trim_end_matches('/'),
                path.trim_end_matches('/'),
            )
        } else if path.contains(['*', '?', '[']) {
            (Kind::Glob, word, path.as_str())
        } else {
            (Kind::File, word, path.as_str())
        };
        out.push(Entry {
            line: number,
            text: text.to_string(),
            path: path.to_string(),
            kind,
            section,
        });
    }
    out
}

/// Whether the glob `pattern` matches the directory `dir` or one above it.
fn glob_owns(pattern: &str, dir: &str) -> bool {
    let pat: Vec<&str> = pattern.split('/').collect();
    let dir: Vec<&str> = dir.split('/').collect();
    dir.len() >= pat.len() && pat.iter().zip(&dir).all(|(p, d)| segment_matches(p, d))
}

/// `*`, `?` and `[...]` classes (without ranges' negation subtleties).
fn segment_matches(pattern: &str, text: &str) -> bool {
    fn go(p: &[char], t: &[char]) -> bool {
        match p.first() {
            None => t.is_empty(),
            Some('*') => (0..=t.len()).any(|i| go(&p[1..], &t[i..])),
            Some('?') => !t.is_empty() && go(&p[1..], &t[1..]),
            Some('[') => match p.iter().position(|c| *c == ']') {
                Some(close) if !t.is_empty() => {
                    let set = &p[1..close];
                    let hit = set.iter().enumerate().any(|(i, c)| {
                        *c == t[0]
                            || (*c == '-'
                                && i > 0
                                && i + 1 < set.len()
                                && set[i - 1] <= t[0]
                                && t[0] <= set[i + 1])
                    });
                    hit && go(&p[close + 1..], &t[1..])
                }
                _ => false,
            },
            Some(c) => !t.is_empty() && *c == t[0] && go(&p[1..], &t[1..]),
        }
    }
    let (p, t): (Vec<char>, Vec<char>) = (pattern.chars().collect(), text.chars().collect());
    go(&p, &t)
}

/// The directories above `text`, nearest first, each as written.
pub(crate) fn parents_of(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut cur = text;
    while let Some(i) = cur.rfind('/') {
        cur = &cur[..i];
        if cur.is_empty() {
            break;
        }
        out.push(cur);
    }
    out
}

/// What the `%files` sections of a spec list.
pub(crate) struct Listing {
    pub(crate) facts: Facts,
    pub(crate) entries: Vec<Entry>,
    pub(crate) sections: Vec<Section>,
    /// A `%files -f <list>` section: the list may own directories.
    pub(crate) has_list: bool,
}

/// Reads every `%files` section of `spec`.
pub(crate) fn list_files(spec: &str) -> Listing {
    let lines: Vec<&str> = spec.split('\n').map(|l| l.trim_end_matches('\r')).collect();
    let facts = facts(&lines);
    let mut entries = Vec::new();
    let mut sections: Vec<Section> = Vec::new();
    let mut current: Option<usize> = None;
    let mut has_list = false;
    let mut depth = 0usize;
    let mut number = 0;
    while number < lines.len() {
        let start = number;
        let line = lines[start];
        number += 1;
        if is_section(line) {
            current = None;
            depth = 0;
            if first_word(line) == "%files" {
                has_list |= line.split_whitespace().skip(1).any(|w| w.starts_with("-f"));
                sections.push(Section {
                    package: section_package(line, &facts),
                    docs: false,
                    last: start,
                });
                current = Some(sections.len() - 1);
            }
            continue;
        }
        let word = first_word(line);
        let Some(section) = current else {
            continue;
        };
        if word.starts_with("%if") {
            depth += 1;
        } else if word == "%endif" {
            depth = depth.saturating_sub(1);
        }
        if word.is_empty()
            || word.starts_with('#')
            || word.starts_with("%if")
            || word.starts_with("%el")
            || word == "%endif"
        {
            continue;
        }
        // Only an entry outside every conditional is a place to add a line
        // that must hold for every build.
        if depth == 0 {
            sections[section].last = start;
        }
        // A line ending in a backslash continues on the next line. The two
        // are read as one line, which starts at `start`, and the backslash
        // is not kept in the word.
        let mut text = line.to_string();
        while let Some(head) = text.trim_end().strip_suffix('\\') {
            text = head.trim_end().to_string();
            match lines.get(number) {
                Some(next) if !is_section(next) => {
                    text.push(' ');
                    text.push_str(next);
                    if depth == 0 {
                        sections[section].last = number;
                    }
                    number += 1;
                }
                _ => break,
            }
        }
        entries.extend(entries_of(&text, start, section, &facts, &mut sections));
    }
    Listing {
        facts,
        entries,
        sections,
        has_list,
    }
}

/// The directories the `%files` sections of `spec` list files in and do not
/// own, in the order of the entries, outermost first for one entry.
pub fn missing_dirs(spec: &str) -> Vec<MissingDir> {
    let Listing {
        facts,
        entries,
        sections,
        has_list,
    } = list_files(spec);
    if has_list {
        return Vec::new();
    }
    let paths: Vec<&str> = entries.iter().map(|e| e.path.as_str()).collect();
    let owns_tree = |e: &Entry| {
        e.kind == Kind::Tree
            || (e.kind != Kind::Glob
                && paths
                    .iter()
                    .any(|p| p.starts_with(e.path.as_str()) && p[e.path.len()..].starts_with('/')))
    };
    let trees: Vec<&str> = entries
        .iter()
        .filter(|e| owns_tree(e))
        .map(|e| e.path.as_str())
        .collect();
    let owned = |dir: &str, section: usize| {
        implicit_owner(&sections[section], dir)
            || entries.iter().any(|e| match e.kind {
                Kind::Dir => e.path == dir,
                Kind::Glob => glob_owns(&e.path, dir),
                _ => false,
            })
            || trees
                .iter()
                .any(|t| dir == *t || (dir.starts_with(t) && dir[t.len()..].starts_with('/')))
    };
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for entry in &entries {
        let base = match entry.kind {
            Kind::Glob => entry
                .text
                .find(['*', '?', '['])
                .map_or(entry.text.as_str(), |i| &entry.text[..i]),
            _ => entry.text.as_str(),
        };
        let mut missing = Vec::new();
        for text in parents_of(base) {
            let Some(path) = expand(text, &facts) else {
                break;
            };
            if is_standard_dir(&path) {
                break;
            }
            if !owned(&path, entry.section) && seen.insert(path.clone()) {
                missing.push(MissingDir {
                    line: entry.line,
                    text: text.to_string(),
                    path,
                });
            }
        }
        missing.reverse();
        out.extend(missing);
    }
    out
}

/// `spec` with a `%dir` line for each directory [`missing_dirs`] finds,
/// outermost first, right before the first `%files` entry under it, so the
/// entry's subpackage and `%if` block are kept. No line is removed or
/// reordered; a second run adds nothing.
pub fn own_new_dirs(spec: &str) -> String {
    let missing = missing_dirs(spec);
    if missing.is_empty() {
        return spec.to_string();
    }
    let mut out = String::with_capacity(spec.len() + missing.len() * 40);
    for (number, line) in spec.split('\n').enumerate() {
        if number > 0 {
            out.push('\n');
        }
        let eol = if line.ends_with('\r') { "\r\n" } else { "\n" };
        for dir in missing.iter().filter(|m| m.line == number) {
            out.push_str("%dir ");
            out.push_str(&dir.text);
            out.push_str(eol);
        }
        out.push_str(line);
    }
    out
}
