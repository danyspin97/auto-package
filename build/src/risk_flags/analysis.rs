use super::*;

/// What one side of the change holds: the spec, the `_service` file and the
/// patches the spec names that the side holds, by file name.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Files {
    pub spec: Option<String>,
    pub service: Option<String>,
    pub patches: BTreeMap<String, String>,
}

/// The patch file names `spec` declares (`Patch:`, `Patch0:`, ...), in order,
/// with `%{name}` and `%{version}` expanded. A name that is not a plain file
/// name of the checkout is left out (see [`unanalysed_patches`]).
pub fn patch_names(spec: &str) -> Vec<String> {
    patch_tags(spec)
        .iter()
        .filter_map(|raw| expand_simple(spec, raw))
        .filter(|name| is_plain_name(name))
        .collect()
}

/// The patch names `spec` declares that cannot be read as files of the
/// checkout, as written: a macro other than `%{name}` or `%{version}`, or an
/// absolute name, or one with a `..` step.
pub fn unanalysed_patches(spec: &str) -> Vec<String> {
    patch_tags(spec)
        .into_iter()
        .filter(|raw| !expand_simple(spec, raw).is_some_and(|name| is_plain_name(&name)))
        .collect()
}

/// The names as written after each `Patch` tag of `spec`.
pub(super) fn patch_tags(spec: &str) -> Vec<String> {
    spec.lines()
        .filter_map(|line| {
            let patch = crate::spec::parse_source_line(line, &["patch"], str::to_string)?;
            patch.url.split_whitespace().next().map(str::to_string)
        })
        .collect()
}

/// `raw` with `%{name}` and `%{version}` replaced by the spec's own `Name` and
/// `Version`. `None` when a macro is left or a tag is missing.
pub(super) fn expand_simple(spec: &str, raw: &str) -> Option<String> {
    let mut out = raw.to_string();
    for (mac, tag) in [("%{name}", "Name"), ("%{version}", "Version")] {
        if out.contains(mac) {
            out = out.replace(mac, &tag_value(spec, tag)?);
        }
    }
    (!out.contains('%')).then_some(out)
}

/// The value of the `tag` line of `spec` (`Name`, `Version`).
pub(super) fn tag_value(spec: &str, tag: &str) -> Option<String> {
    spec.lines().find_map(|line| {
        let (t, rest) = line.split_once(':')?;
        t.trim()
            .eq_ignore_ascii_case(tag)
            .then(|| rest.trim().to_string())
    })
}

/// Whether `name` can be a file of the checkout: relative, with no `..` step.
pub(super) fn is_plain_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('/')
        && !name.contains('\0')
        && !name.split('/').any(|c| c == "..")
}

/// The `name` of each `<service>` element of a `_service` file.
pub(super) fn service_names(text: &str) -> Vec<String> {
    text.match_indices("<service")
        .filter_map(|(pos, _)| {
            let rest = &text[pos + "<service".len()..];
            let tag = &rest[..rest.find('>')?];
            crate::obs_xml::attr(tag, "name")
        })
        .collect()
}

/// The body lines of each scriptlet of `lines`, by header: the live lines
/// after it, up to the next section or scriptlet header.
pub(super) fn scriptlet_bodies(lines: &[&str]) -> BTreeMap<String, Vec<String>> {
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut current: Option<String> = None;
    for line in lines {
        if let Some(header) = headers(std::slice::from_ref(line)).into_iter().next() {
            out.entry(header.clone()).or_default();
            current = Some(header);
            continue;
        }
        let t = line.trim();
        if is_section(t) {
            current = None;
        } else if let Some(body) = current.as_ref().and_then(|h| out.get_mut(h))
            && !t.is_empty()
            && !t.starts_with('#')
        {
            body.push(t.to_string());
        }
    }
    out
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

    // Scriptlets: a header the baseline does not have is a new scriptlet; a
    // body changed under a header it has is a changed one.
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
    let base_bodies = scriptlet_bodies(&base_spec);
    for (header, body) in scriptlet_bodies(&fin_spec) {
        if base_bodies.get(&header).is_some_and(|old| *old != body) {
            push(RiskFlag::ChangedScriptlet { header });
        }
    }

    // Services: a `<service>` the `_service` file gains.
    let base_services = service_names(base.service.as_deref().unwrap_or(""));
    for name in service_names(fin.service.as_deref().unwrap_or("")) {
        if !base_services.contains(&name) {
            push(RiskFlag::NewService { name });
        }
    }

    // Patch names that cannot be read as files of the checkout are not
    // analysed, and say so: the ones the change adds, not the ones the
    // baseline already names the same way.
    let base_unread = unanalysed_patches(base.spec.as_deref().unwrap_or(""));
    for raw in unanalysed_patches(&fin_spec.join("\n")) {
        if !base_unread.contains(&raw) {
            push(RiskFlag::Unanalysed {
                what: format!("patch {raw} is not a plain file name of the checkout"),
            });
        }
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
/// Scriptlets the spec may declare, by the word after `%`.
pub(super) const SCRIPTLETS: &[&str] = &[
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
pub(super) fn headers(lines: &[&str]) -> Vec<String> {
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
pub(super) fn changed_lines<'a>(a: &[&'a str], b: &[&str]) -> Vec<&'a str> {
    let live = |l: &&str| {
        let t = l.trim();
        !t.is_empty() && !t.starts_with('#')
    };
    let a: Vec<&str> = a.iter().copied().filter(live).collect();
    let b: Vec<&str> = b.iter().copied().filter(live).collect();
    extra(&a, &b).into_iter().map(|i| a[i]).collect()
}

/// Whether a line expands a shell command or Lua code.
pub(super) fn expands(line: &str) -> bool {
    line.contains("%(") || line.contains("%{lua")
}

/// Whether a line is a `Provides` or a `Requires(...)` that runs in a
/// scriptlet phase (`pre`, `post`, `preun`, `postun`, `pretrans`,
/// `posttrans`, `verify`).
pub(super) fn is_script_dependency(line: &str) -> bool {
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
pub(super) fn hosts(files: &Files) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for line in files.spec.as_deref().unwrap_or("").lines() {
        if crate::spec::parse_source_line(line, &["source", "patch"], str::to_string).is_some()
            && let Some((_, rest)) = line.split_once(':')
        {
            out.extend(url_hosts(rest));
        }
    }
    out.extend(url_hosts(files.service.as_deref().unwrap_or("")));
    out
}

/// The host of every `scheme://host` URL in `text`, lower case.
pub(super) fn url_hosts(text: &str) -> Vec<String> {
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
pub(super) fn patch_paths(text: &str) -> BTreeSet<String> {
    let lines: Vec<&str> = text.lines().collect();
    let mut out = BTreeSet::new();
    // A git patch names its files in `diff --git a/x b/x` headers, which need
    // not be followed by `---`/`+++` lines (a new or renamed empty file).
    for line in &lines {
        if let Some(rest) = line.strip_prefix("diff --git ")
            && let Some(b) = rest.split_whitespace().nth(1)
        {
            let path = b.strip_prefix("b/").unwrap_or(b);
            if path != "/dev/null" && !path.is_empty() {
                out.insert(path.to_string());
            }
        }
    }
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
