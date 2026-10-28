//! What a package's change adds that a reviewer must look at before it is
//! submitted: a scriptlet the baseline did not have, a source host it did not
//! name, shell or Lua expansion, a changed `Requires(pre/post...)` or
//! `Provides` line, and a patch that touches a file neither the baseline's
//! patches nor the source archive have.
//!
//! Like the fix-loop flags (`fix_flags`), these are read from the diff of the
//! OBS baseline against the final files, never from what an agent said. They
//! are flags, not refusals: `--yes` is what refuses to submit them.
//!
//! The baseline is the spec OBS holds. A package OBS does not hold has no
//! baseline, so every line counts as new for it.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::fix_flags::{extra, one_line};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RiskFlag {
    /// A scriptlet the baseline does not have, by its header (`%post -n foo`).
    NewScriptlet { header: String },
    /// A host that a `Source`/`Patch` URL or the `_service` file names and the
    /// baseline does not.
    NewSourceHost { host: String },
    /// A line that expands a shell command (`%(`) or Lua (`%{lua`), added or
    /// changed.
    ShellExpansion { line: String },
    /// A `Requires(pre|post|preun|postun...)` or `Provides` line that the
    /// change added (`added`) or removed.
    ScriptDependency { added: bool, line: String },
    /// A patch that was added or changed and touches `path`, which neither
    /// the baseline's patches nor the source archive have.
    PatchOutsideSet { patch: String, path: String },
}

impl RiskFlag {
    /// The one line that says it, the same wherever it is printed.
    pub fn line(&self) -> String {
        let text = match self {
            RiskFlag::NewScriptlet { header } => format!("RISK: new scriptlet: {header}"),
            RiskFlag::NewSourceHost { host } => format!("RISK: new source host: {host}"),
            RiskFlag::ShellExpansion { line } => format!("RISK: shell or Lua expansion: {line}"),
            RiskFlag::ScriptDependency { added, line } => {
                let change = if *added { "added" } else { "removed" };
                format!("RISK: {change} dependency line: {line}")
            }
            RiskFlag::PatchOutsideSet { patch, path } => format!(
                "RISK: {patch} touches {path}, which no existing patch and no file of the \
                 source archive has"
            ),
        };
        one_line(&text)
    }
}

/// What one side of the change holds: the spec, the `_service` file and the
/// patches the spec names that the side holds, by file name.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Files {
    pub spec: Option<String>,
    pub service: Option<String>,
    pub patches: BTreeMap<String, String>,
}

/// The patch file names `spec` declares (`Patch:`, `Patch0:`, ...), in order.
pub fn patch_names(spec: &str) -> Vec<String> {
    spec.lines()
        .filter_map(|line| {
            let (tag, rest) = line.split_once(':')?;
            if !is_tag(tag, "patch") {
                return None;
            }
            rest.split_whitespace().next().map(str::to_string)
        })
        .collect()
}

/// The files of the source archive `tarball` holds, without its top directory
/// (`foo-1.0/setup.py` is `setup.py`), which is how a patch names them. Empty
/// when there is no archive or it cannot be listed.
pub fn archive_files(tarball: Option<&Path>) -> Vec<String> {
    let Some(path) = tarball else {
        return Vec::new();
    };
    ap_ecosystem::sdist::list_archive_members(path)
        .unwrap_or_default()
        .into_iter()
        .filter(|m| !m.ends_with('/'))
        .filter_map(|m| m.split_once('/').map(|(_, rest)| rest.to_string()))
        .collect()
}

/// The risk flags of a change from `base` (what OBS holds) to `fin` (what is
/// to be committed). `archive` lists the files of the source archive of the
/// new version, see [`archive_files`].
pub fn from_change(base: &Files, fin: &Files, archive: &[String]) -> Vec<RiskFlag> {
    let base_spec: Vec<&str> = base.spec.as_deref().unwrap_or("").lines().collect();
    let fin_spec: Vec<&str> = fin.spec.as_deref().unwrap_or("").lines().collect();
    let mut out: Vec<RiskFlag> = Vec::new();
    let mut push = |flag: RiskFlag| {
        if !out.contains(&flag) {
            out.push(flag);
        }
    };

    // Scriptlets: a header the baseline does not have. A body changed under an
    // existing header is not a new scriptlet.
    let base_headers = headers(&base_spec);
    let fin_headers = headers(&fin_spec);
    for i in extra(
        &fin_headers.iter().map(String::as_str).collect::<Vec<_>>(),
        &base_headers.iter().map(String::as_str).collect::<Vec<_>>(),
    ) {
        push(RiskFlag::NewScriptlet {
            header: fin_headers[i].clone(),
        });
    }

    // Expansion and dependency lines: the lines the change added or removed,
    // comments aside.
    let added = changed_lines(&fin_spec, &base_spec);
    let removed = changed_lines(&base_spec, &fin_spec);
    for line in &added {
        if expands(line) {
            push(RiskFlag::ShellExpansion {
                line: one_line(line),
            });
        }
    }
    for (is_added, lines) in [(true, &added), (false, &removed)] {
        for line in lines.iter().filter(|l| is_script_dependency(l)) {
            push(RiskFlag::ScriptDependency {
                added: is_added,
                line: one_line(line),
            });
        }
    }

    // Source hosts: every host the final files name that the baseline does not.
    let base_hosts = hosts(base);
    for host in hosts(fin) {
        if !base_hosts.contains(&host) {
            push(RiskFlag::NewSourceHost { host });
        }
    }

    // Patches: a patch that was added or changed may only touch files the
    // baseline's patches or the source archive have.
    let known: BTreeSet<String> = base.patches.values().flat_map(|t| patch_paths(t)).collect();
    let tree: BTreeSet<&str> = archive.iter().map(String::as_str).collect();
    let is_known = |path: &str| {
        // A patch may name the top directory of the archive (`foo-1.0/x.py`)
        // that the archive listing leaves out.
        let bare = path.split_once('/').map(|(_, rest)| rest);
        [Some(path), bare]
            .into_iter()
            .flatten()
            .any(|p| known.contains(p) || tree.contains(p))
    };
    for (patch, text) in &fin.patches {
        if base.patches.get(patch) == Some(text) {
            continue;
        }
        for path in patch_paths(text) {
            if !is_known(&path) {
                push(RiskFlag::PatchOutsideSet {
                    patch: patch.clone(),
                    path,
                });
            }
        }
    }
    out
}

/// Whether `tag` (the text before a colon) is `name` with an optional number
/// (`Source`, `Source0`, `patch12`).
fn is_tag(tag: &str, name: &str) -> bool {
    let tag = tag.trim().to_ascii_lowercase();
    tag.strip_prefix(name)
        .is_some_and(|n| n.bytes().all(|b| b.is_ascii_digit()))
}

/// Scriptlets the spec may declare, by the word after `%`.
const SCRIPTLETS: &[&str] = &[
    "pre",
    "post",
    "preun",
    "postun",
    "pretrans",
    "posttrans",
    "verifyscript",
    "triggerprein",
    "triggerin",
    "triggerun",
    "triggerpostun",
    "filetriggerin",
    "filetriggerun",
    "filetriggerpostun",
    "transfiletriggerin",
    "transfiletriggerun",
    "transfiletriggerpostun",
];

/// The scriptlet headers of `lines`, spaces collapsed: `%post -n foo`.
fn headers(lines: &[&str]) -> Vec<String> {
    lines
        .iter()
        .filter_map(|l| {
            let t = l.trim();
            let word = t.split_whitespace().next()?.strip_prefix('%')?;
            SCRIPTLETS
                .contains(&word.to_ascii_lowercase().as_str())
                .then(|| t.split_whitespace().collect::<Vec<_>>().join(" "))
        })
        .collect()
}

/// The lines of `a` that `b` does not have, comments and blank lines aside.
fn changed_lines<'a>(a: &[&'a str], b: &[&str]) -> Vec<&'a str> {
    let live = |l: &&str| {
        let t = l.trim();
        !t.is_empty() && !t.starts_with('#')
    };
    let a: Vec<&str> = a.iter().copied().filter(live).collect();
    let b: Vec<&str> = b.iter().copied().filter(live).collect();
    extra(&a, &b).into_iter().map(|i| a[i]).collect()
}

/// Whether a line expands a shell command or Lua code.
fn expands(line: &str) -> bool {
    line.contains("%(") || line.contains("%{lua")
}

/// Whether a line is a `Provides` or a `Requires(...)` that runs in a
/// scriptlet phase (`pre`, `post`, `preun`, `postun`, `pretrans`,
/// `posttrans`, `verify`).
fn is_script_dependency(line: &str) -> bool {
    const PHASES: &[&str] = &[
        "pre",
        "post",
        "preun",
        "postun",
        "pretrans",
        "posttrans",
        "verify",
    ];
    let t = line.trim().to_ascii_lowercase();
    if let Some(rest) = t.strip_prefix("provides") {
        return rest.is_empty() || rest.starts_with(':') || rest.starts_with(char::is_whitespace);
    }
    let Some(rest) = t.strip_prefix("requires(") else {
        return false;
    };
    rest.split_once(')')
        .is_some_and(|(phases, _)| phases.split(',').any(|p| PHASES.contains(&p.trim())))
}

/// The hosts of the `Source`/`Patch` URLs of the spec and of the `_service`
/// URLs, for one side of the change.
fn hosts(files: &Files) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for line in files.spec.as_deref().unwrap_or("").lines() {
        if let Some((tag, rest)) = line.split_once(':')
            && (is_tag(tag, "source") || is_tag(tag, "patch"))
        {
            out.extend(url_hosts(rest));
        }
    }
    out.extend(url_hosts(files.service.as_deref().unwrap_or("")));
    out
}

/// The host of every `scheme://host` URL in `text`, lower case.
fn url_hosts(text: &str) -> Vec<String> {
    text.match_indices("://")
        .filter_map(|(pos, _)| {
            let rest = &text[pos + 3..];
            let end = rest
                .find(|c: char| {
                    matches!(c, '/' | '?' | '#' | '"' | '\'' | '<' | '>') || c.is_whitespace()
                })
                .unwrap_or(rest.len());
            let authority = &rest[..end];
            let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
            let host = host.split(':').next().unwrap_or("").to_ascii_lowercase();
            (!host.is_empty()).then_some(host)
        })
        .collect()
}

/// The paths a unified diff changes, as `patch -p1` names them: the `+++`
/// line of each file header (a `---` line directly before it), without the
/// `b/` prefix and without `/dev/null`.
fn patch_paths(text: &str) -> BTreeSet<String> {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = BTreeSet::new();
    for pair in lines.windows(2) {
        let (old, new) = (pair[0], pair[1]);
        if !old.starts_with("--- ") {
            continue;
        }
        let Some(rest) = new.strip_prefix("+++ ") else {
            continue;
        };
        let path = rest.split('\t').next().unwrap_or("").trim();
        let path = path.strip_prefix("b/").unwrap_or(path);
        if !path.is_empty() && path != "/dev/null" {
            out.insert(path.to_string());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPEC: &str = "Name: python-foo\nVersion: 1.0\nSource0: https://files.example.org/foo-1.0.tar.gz\n\
        Patch0: fix-build.patch\n%description\nFoo.\n%prep\n%setup -q\n%build\n%py3_build\n\
        %install\n%py3_install\n%files\n%{python3_sitelib}/foo\n";

    const PATCH: &str = "--- foo-1.0/setup.py\t2020-01-01\n+++ foo-1.0/setup.py\t2020-01-01\n\
        @@ -1 +1 @@\n-a\n+b\n";

    fn files(spec: &str) -> Files {
        Files {
            spec: Some(spec.to_string()),
            service: None,
            patches: BTreeMap::new(),
        }
    }

    fn with(replace: &str, with: &str) -> String {
        SPEC.replace(replace, with)
    }

    fn flags(base: &str, fin: &str) -> Vec<RiskFlag> {
        from_change(&files(base), &files(fin), &[])
    }

    #[test]
    fn an_unchanged_package_has_no_risk() {
        assert!(flags(SPEC, SPEC).is_empty());
    }

    #[test]
    fn a_new_scriptlet_is_flagged_by_its_header() {
        let fin = with("%install\n", "%post -n python3-foo\n%install\n");
        assert_eq!(
            flags(SPEC, &fin),
            vec![RiskFlag::NewScriptlet {
                header: "%post -n python3-foo".into()
            }]
        );
    }

    #[test]
    fn a_changed_body_under_an_existing_scriptlet_is_not_a_new_one() {
        let base = with("%install\n", "%post\necho a\n%install\n");
        let fin = with("%install\n", "%post\necho b\n%install\n");
        assert!(
            flags(&base, &fin)
                .iter()
                .all(|f| !matches!(f, RiskFlag::NewScriptlet { .. }))
        );
    }

    #[test]
    fn a_scriptlet_named_in_a_comment_is_not_one() {
        let fin = with("%install\n", "# %post is not used here\n%install\n");
        assert!(flags(SPEC, &fin).is_empty());
    }

    #[test]
    fn a_new_source_host_is_flagged_and_a_known_one_is_not() {
        let same = with(
            "Source0: https://files.example.org/foo-1.0.tar.gz",
            "Source0: https://files.example.org/foo-%{version}.tar.gz",
        );
        assert!(flags(SPEC, &same).is_empty());
        let moved = with(
            "https://files.example.org/foo-1.0.tar.gz",
            "https://GitHub.com:443/foo/foo/archive/v1.0.tar.gz",
        );
        assert_eq!(
            flags(SPEC, &moved),
            vec![RiskFlag::NewSourceHost {
                host: "github.com".into()
            }]
        );
    }

    #[test]
    fn a_patch_url_host_counts_and_a_userinfo_is_stripped() {
        let fin = with(
            "Patch0: fix-build.patch",
            "Patch0: https://user:pw@patches.example.net/fix.patch",
        );
        assert_eq!(
            flags(SPEC, &fin),
            vec![RiskFlag::NewSourceHost {
                host: "patches.example.net".into()
            }]
        );
    }

    #[test]
    fn a_service_url_on_a_new_host_is_flagged() {
        let base = Files {
            spec: Some(SPEC.into()),
            service: Some(
                "<service name=\"download_url\"><param name=\"host\">files.example.org</param></service>"
                    .into(),
            ),
            patches: BTreeMap::new(),
        };
        let fin = Files {
            spec: Some(SPEC.into()),
            service: Some(
                "<service name=\"download_url\"><param name=\"url\">https://evil.example.com/x</param></service>"
                    .into(),
            ),
            patches: BTreeMap::new(),
        };
        assert_eq!(
            from_change(&base, &fin, &[]),
            vec![RiskFlag::NewSourceHost {
                host: "evil.example.com".into()
            }]
        );
    }

    #[test]
    fn shell_and_lua_expansion_added_are_flagged() {
        let fin = with(
            "%py3_build\n",
            "%py3_build\n%global rev %(git rev-parse HEAD)\n%{lua: print(1)}\n",
        );
        let found = flags(SPEC, &fin);
        assert_eq!(
            found,
            vec![
                RiskFlag::ShellExpansion {
                    line: "%global rev %(git rev-parse HEAD)".into()
                },
                RiskFlag::ShellExpansion {
                    line: "%{lua: print(1)}".into()
                },
            ]
        );
    }

    #[test]
    fn an_expansion_already_in_the_baseline_is_not_added() {
        let base = with("%py3_build\n", "%py3_build\n%global rev %(date)\n");
        assert!(flags(&base, &base).is_empty());
    }

    #[test]
    fn a_script_requires_or_a_provides_changed_is_flagged_both_ways() {
        let base = with(
            "%description\n",
            "Requires(post): old-helper\nProvides: foo = 1.0\n%description\n",
        );
        let fin = with(
            "%description\n",
            "Requires(post,postun): new-helper\nProvides: foo = 2.0\n%description\n",
        );
        let found = flags(&base, &fin);
        assert!(found.contains(&RiskFlag::ScriptDependency {
            added: true,
            line: "Requires(post,postun): new-helper".into()
        }));
        assert!(found.contains(&RiskFlag::ScriptDependency {
            added: false,
            line: "Requires(post): old-helper".into()
        }));
        assert!(found.contains(&RiskFlag::ScriptDependency {
            added: true,
            line: "Provides: foo = 2.0".into()
        }));
        assert!(found.contains(&RiskFlag::ScriptDependency {
            added: false,
            line: "Provides: foo = 1.0".into()
        }));
        assert_eq!(found.len(), 4);
    }

    #[test]
    fn a_plain_requires_is_not_a_script_dependency() {
        let fin = with(
            "%description\n",
            "Requires: bar\nRequires(pre): baz\n%description\n",
        );
        let found = flags(SPEC, &fin);
        assert_eq!(
            found,
            vec![RiskFlag::ScriptDependency {
                added: true,
                line: "Requires(pre): baz".into()
            }]
        );
    }

    #[test]
    fn a_patch_added_outside_the_known_files_is_flagged() {
        let fin_patch = "--- foo-1.0/etc/passwd\n+++ foo-1.0/etc/passwd\n@@ -1 +1 @@\n-a\n+b\n";
        let mut fin = files(SPEC);
        fin.patches
            .insert("fix-build.patch".into(), fin_patch.into());
        let found = from_change(&files(SPEC), &fin, &["setup.py".into()]);
        assert_eq!(
            found,
            vec![RiskFlag::PatchOutsideSet {
                patch: "fix-build.patch".into(),
                path: "foo-1.0/etc/passwd".into()
            }]
        );
    }

    #[test]
    fn a_patch_touching_a_file_of_the_archive_or_of_a_baseline_patch_is_not_flagged() {
        let mut base = files(SPEC);
        base.patches.insert("fix-build.patch".into(), PATCH.into());
        let mut fin = files(SPEC);
        let refreshed = "--- foo-1.0/setup.py\n+++ foo-1.0/setup.py\n@@ -1 +1 @@\n-a\n+c\n\
             --- foo-1.0/src/lib.c\n+++ b/src/lib.c\n@@ -1 +1 @@\n-a\n+c\n";
        fin.patches
            .insert("fix-build.patch".into(), refreshed.into());
        // setup.py is a baseline patch's file, src/lib.c is in the archive.
        assert!(from_change(&base, &fin, &["src/lib.c".into()]).is_empty());
    }

    #[test]
    fn an_unchanged_patch_with_an_unknown_file_is_not_flagged() {
        let odd = "--- a/x\n+++ b/x/unknown\n@@ -1 +1 @@\n-a\n+b\n";
        let mut base = files(SPEC);
        base.patches.insert("odd.patch".into(), odd.into());
        let mut fin = files(SPEC);
        fin.patches.insert("odd.patch".into(), odd.into());
        assert!(from_change(&base, &fin, &[]).is_empty());
    }

    #[test]
    fn a_plus_line_that_looks_like_a_header_is_not_a_file() {
        // A hunk adding "++ b/other" reads as "+++ b/other" but follows no "---".
        let text = "--- a/x\n+++ b/x\n@@ -1,2 +1,2 @@\n-a\n++ b/other\n";
        assert_eq!(patch_paths(text), BTreeSet::from(["x".to_string()]));
    }

    #[test]
    fn archive_members_lose_their_top_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("foo-1.0.tar.gz");
        let file = std::fs::File::create(&path).unwrap();
        let enc = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
        let mut tar = tar::Builder::new(enc);
        for name in ["foo-1.0/setup.py", "foo-1.0/src/lib.c"] {
            let mut h = tar::Header::new_gnu();
            h.set_size(1);
            h.set_mode(0o644);
            h.set_cksum();
            tar.append_data(&mut h, name, &b"x"[..]).unwrap();
        }
        tar.into_inner().unwrap().finish().unwrap();
        let files = archive_files(Some(&path));
        assert!(files.contains(&"setup.py".to_string()));
        assert!(files.contains(&"src/lib.c".to_string()));
        assert!(archive_files(None).is_empty());
    }

    #[test]
    fn patch_names_come_from_the_spec_in_order() {
        let spec = "Patch: a.patch\nPatch12: b.patch\nSource: x.tar.gz\n";
        assert_eq!(patch_names(spec), vec!["a.patch", "b.patch"]);
    }

    #[test]
    fn a_line_is_one_line_and_capped() {
        let line = RiskFlag::ShellExpansion {
            line: "%(x) ".repeat(200),
        }
        .line();
        assert!(!line.contains('\n'));
        assert!(line.starts_with("RISK: "));
        assert!(line.ends_with("..."));
    }
}
