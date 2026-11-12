//! The `%dir` lines a failed build asks for, added without an agent turn.
//!
//! OBS lists the directories no package owns in the build log
//! ([`crate::log::unowned`]). Each one gets a `%dir` line directly above the
//! first `%files` entry inside it, spelled as that entry spells its path, with
//! no comment. A directory no entry lies in goes to the end of the `%files`
//! section whose entries share the longest prefix with it, spelled with the
//! macros (`/usr/share` is `%{_datadir}`).

use crate::spec_tidy::dir_owners::{Entry, Facts, Kind, Listing, expand, list_files, parents_of};
use crate::spec_tidy::standard_dirs::is_standard_dir;

/// The standard macros, longest value first so the nearest one wins.
const SPELLINGS: &[(&str, &str)] = &[
    ("/usr/lib/systemd/system", "%{_unitdir}"),
    ("/usr/lib/systemd/user", "%{_userunitdir}"),
    ("/usr/lib/sysusers.d", "%{_sysusersdir}"),
    ("/usr/lib/tmpfiles.d", "%{_tmpfilesdir}"),
    ("/usr/lib/udev/rules.d", "%{_udevrulesdir}"),
    ("/usr/share/man", "%{_mandir}"),
    ("/usr/share/info", "%{_infodir}"),
    ("/usr/share/doc/packages", "%{_docdir}"),
    ("/usr/share", "%{_datadir}"),
    ("/usr/libexec", "%{_libexecdir}"),
    ("/usr/include", "%{_includedir}"),
    ("/usr/lib64", "%{_libdir}"),
    ("/usr/sbin", "%{_sbindir}"),
    ("/usr/bin", "%{_bindir}"),
    ("/var/lib", "%{_sharedstatedir}"),
    ("/var", "%{_localstatedir}"),
    ("/etc", "%{_sysconfdir}"),
    ("/usr", "%{_prefix}"),
];

/// `path` with its leading directories written as macros.
fn spell(path: &str) -> String {
    for (dir, name) in SPELLINGS {
        if path == *dir {
            return (*name).to_string();
        }
        if let Some(rest) = path.strip_prefix(dir)
            && rest.starts_with('/')
        {
            return format!("{name}{rest}");
        }
    }
    path.to_string()
}

/// Where one `%dir` line goes: before line `at` (0-based).
struct Insert {
    at: usize,
    depth: usize,
    text: String,
}

/// The number of leading path components `a` and `b` share.
fn common(a: &str, b: &str) -> usize {
    a.split('/')
        .zip(b.split('/'))
        .take_while(|(x, y)| x == y)
        .count()
}

/// The `%dir` line for `dir` placed by `entry_text`, the text of an entry
/// inside it: the parent of that text that expands to `dir`.
fn spelled_like(entry_text: &str, dir: &str, facts: &Facts) -> Option<String> {
    let base = entry_text
        .find(['*', '?', '['])
        .map_or(entry_text, |i| &entry_text[..i]);
    parents_of(base)
        .into_iter()
        .find(|p| expand(p, facts).as_deref() == Some(dir))
        .map(str::to_string)
}

/// The spec with its new `%dir` lines.
pub struct UnownedFix {
    pub spec: String,
    /// The directories of the new lines, as they are written there.
    pub added: Vec<String>,
}

/// `spec` with a `%dir` line for each of `paths` (the unowned directories of
/// a build log), or `None` when it needs none: every path is owned already or
/// no section could take it. A path that is not absolute, or has whitespace,
/// a quote or a macro character, is left alone. A second run changes nothing.
pub fn own_unowned_dirs(spec: &str, paths: &[String]) -> Option<UnownedFix> {
    let Listing {
        facts,
        entries,
        sections,
        ..
    } = list_files(spec);
    let mut inserts: Vec<Insert> = Vec::new();
    let mut done: Vec<&str> = Vec::new();
    for dir in paths {
        let dir = dir.trim_end_matches('/');
        if !dir.starts_with('/')
            || dir.contains(|c: char| c.is_whitespace() || "%$\"'*?[".contains(c))
            || done.contains(&dir)
        {
            continue;
        }
        done.push(dir);
        if is_standard_dir(dir) {
            continue;
        }
        let depth = dir.matches('/').count();
        let inside = |e: &&Entry| {
            let literal = e
                .path
                .find(['*', '?', '['])
                .map_or(e.path.as_str(), |i| &e.path[..i]);
            literal.starts_with(&format!("{dir}/"))
        };
        let owns = |section: usize| {
            entries.iter().any(|e| {
                e.section == section
                    && e.kind != Kind::File
                    && e.kind != Kind::Glob
                    && e.path == dir
            })
        };
        // Every section with an entry inside the directory owns it itself.
        let mut sections_inside: Vec<&Entry> = Vec::new();
        for e in entries.iter().filter(inside) {
            if !sections_inside.iter().any(|f| f.section == e.section) {
                sections_inside.push(e);
            }
        }
        let mut handled = false;
        for first in sections_inside {
            if owns(first.section) {
                handled = true;
            } else if let Some(text) = spelled_like(&first.text, dir, &facts) {
                handled = true;
                inserts.push(Insert {
                    at: first.line,
                    depth,
                    text: format!("%dir {text}"),
                });
            }
        }
        if handled {
            continue;
        }
        // No entry inside: the section nearest to the directory takes it.
        let best = entries
            .iter()
            .map(|e| (common(&e.path, dir), e.section))
            .max_by_key(|(n, s)| (*n, std::cmp::Reverse(*s)));
        let Some((shared, section)) = best else {
            continue;
        };
        if shared < 2 || owns(section) {
            continue;
        }
        inserts.push(Insert {
            at: sections[section].last + 1,
            depth,
            text: format!("%dir {}", spell(dir)),
        });
    }
    if inserts.is_empty() {
        return None;
    }
    inserts.sort_by_key(|i| (i.at, i.depth));
    let lines: Vec<&str> = spec.split('\n').collect();
    let mut out: Vec<String> = Vec::with_capacity(lines.len() + inserts.len());
    for (number, line) in lines.iter().enumerate() {
        for ins in inserts.iter().filter(|i| i.at == number) {
            out.push(with_eol(&ins.text, line));
        }
        out.push((*line).to_string());
    }
    for ins in inserts.iter().filter(|i| i.at >= lines.len()) {
        let last = lines.last().copied().unwrap_or("");
        out.push(with_eol(&ins.text, last));
    }
    Some(UnownedFix {
        spec: out.join("\n"),
        added: inserts
            .iter()
            .map(|i| i.text.trim_start_matches("%dir ").to_string())
            .collect(),
    })
}

/// `text` with the line ending `like` has.
fn with_eol(text: &str, like: &str) -> String {
    if like.ends_with('\r') {
        format!("{text}\r")
    } else {
        text.to_string()
    }
}
