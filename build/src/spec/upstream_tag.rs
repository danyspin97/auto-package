//! A spec whose upstream tag is not its `Version:` as written: a dashed tag
//! (`v1.0.0-1`) packaged as `Version: 1.0.0.1`, with the tag kept in a define
//! (`%define sillyver 1.0.0-1`) that the `Source:` and `%autosetup -n` use.

use super::expand::{collect_simple_defines, expand_simple_defines};
use super::parse::tag_value;
use super::set_version;

/// `spec` bumped to `version`, for the upstream `tag` it comes from (`None`
/// when the tag is the version as written). A `%define` or `%global` that holds
/// the old tag, the one whose RPM version is the old `Version:`, takes the new
/// tag and keeps its name, so the source it names is the one the version says.
///
/// `Err` says why the spec cannot be bumped: the tag does not mean `version`, or
/// the old tag is held in the spec but no define of the spec can take the new
/// one. Without a define holding the old tag, a plain tag is the version.
pub fn set_version_and_tag(spec: &str, version: &str, tag: Option<&str>) -> Result<String, String> {
    let raw = tag_value(spec, "Version:").ok_or("its Version: is not a plain value")?;
    let defines = collect_simple_defines(spec);
    // The packaged version as the spec computes it (`Version: %{pkgver}`).
    let old = expand_simple_defines(raw, &defines);
    if let Some(tag) = tag {
        match crate::upstream::rpm_version_for_tag(tag) {
            Ok(rpm) if rpm == version => {}
            _ => return Err(format!("upstream tag {tag} is not version {version}")),
        }
    }
    // The defines whose value is a dashed tag of the packaged version.
    let holders: Vec<(String, String)> = defines
        .into_iter()
        .filter(|(_, value)| {
            value.contains('-')
                && crate::upstream::rpm_version_for_tag(value).as_deref() == Ok(old.as_str())
        })
        .collect();
    let plain =
        || set_version(spec, version).ok_or_else(|| "its Version: is not a plain value".into());
    if holders.is_empty() {
        return match tag {
            Some(tag) if tag != version => add_upstream_version(spec, version, tag),
            _ => plain(),
        };
    }
    // A plain tag (`None`) is the version itself: the define takes it as it is.
    let tag = tag.unwrap_or(version);
    let retagged = retag_defines(spec, &holders, tag);
    set_version(&retagged, version).ok_or_else(|| "its Version: is not a plain value".into())
}

/// `spec` for a dashed `tag` that no define holds: `%define upstream_version
/// <tag>` goes under the first `Version:`, and the version macro (`%{version}`
/// or `%version`) becomes `%{upstream_version}` in the one `Source:` line that
/// is the forge's tag archive and in the `%setup`/`%autosetup -n` lines that
/// name the version, and nowhere else. A spec of another shape has no such
/// place, and `Err` says why, a maintainer's job: no tag archive that names the
/// version, a second source that names it too (a vendored tarball would point
/// at a file that is not there), no `-n` that names it, or a `Version:` inside
/// a conditional.
fn add_upstream_version(spec: &str, version: &str, tag: &str) -> Result<String, String> {
    let needs = |why: &str| {
        format!(
            "the upstream tag {tag} of version {version} is not in the spec: {why}; \
             needs a maintainer"
        )
    };
    if collect_simple_defines(spec).contains_key("upstream_version") {
        return Err(needs("a define of that name already exists"));
    }
    let mut depth = 0usize;
    let mut version_at: Option<usize> = None;
    let mut tag_archives: Vec<usize> = Vec::new();
    let mut other_sources = false;
    let mut setups: Vec<usize> = Vec::new();
    for (i, line) in spec.lines().enumerate() {
        let t = line.trim_start();
        if t.starts_with("%if") {
            depth += 1;
        } else if t.starts_with("%endif") {
            depth = depth.saturating_sub(1);
        }
        if line.starts_with("Version:") && version_at.is_none() {
            if depth > 0 {
                return Err(needs("its Version: is inside a conditional"));
            }
            version_at = Some(i);
        }
        let source = t.strip_prefix("Source").is_some_and(|r| {
            r.trim_start_matches(|c: char| c.is_ascii_digit())
                .starts_with(':')
        });
        let setup = (t.starts_with("%setup") || t.starts_with("%autosetup"))
            && t.split_whitespace().any(|w| w == "-n");
        if source && names_version(line) {
            if line.contains("/archive/") || line.contains("/tags/") {
                tag_archives.push(i);
            } else {
                other_sources = true;
            }
        } else if setup && names_version(line) {
            setups.push(i);
        }
    }
    let Some(version_at) = version_at else {
        return Err(needs("it has no Version:"));
    };
    if tag_archives.len() != 1 {
        return Err(needs(
            "its Source: is not one forge archive that names the version",
        ));
    }
    if other_sources {
        return Err(needs("another Source: names the version too"));
    }
    if setups.is_empty() {
        return Err(needs("no %setup or %autosetup -n names the version"));
    }
    let mut lines: Vec<String> = Vec::new();
    for (i, line) in spec.lines().enumerate() {
        if tag_archives.contains(&i) || setups.contains(&i) {
            lines.push(replace_version_macro(line));
        } else {
            lines.push(line.to_string());
        }
        if i == version_at {
            lines.push(format!("%define upstream_version {tag}"));
        }
    }
    let edited = lines.join("\n") + if spec.ends_with('\n') { "\n" } else { "" };
    set_version(&edited, version).ok_or_else(|| "its Version: is not a plain value".into())
}

/// The length of the version macro (`%{version}` or a bare `%version` that
/// does not run into a name) at the start of `s`, if there is one.
fn version_macro_len(s: &str) -> Option<usize> {
    if s.starts_with("%{version}") {
        return Some("%{version}".len());
    }
    let bare = s.strip_prefix("%version")?;
    match bare.chars().next() {
        Some(c) if c.is_ascii_alphanumeric() || c == '_' => None,
        _ => Some("%version".len()),
    }
}

/// Whether `line` names the version, in either spelling.
fn names_version(line: &str) -> bool {
    line.match_indices('%')
        .any(|(i, _)| version_macro_len(&line[i..]).is_some())
}

/// `line` with each version macro, in either spelling, as `%{upstream_version}`.
fn replace_version_macro(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut i = 0;
    while i < line.len() {
        let rest = &line[i..];
        match version_macro_len(rest) {
            Some(n) => {
                out.push_str("%{upstream_version}");
                i += n;
            }
            None => {
                let c = rest.chars().next().unwrap_or_default();
                out.push(c);
                i += c.len_utf8();
            }
        }
    }
    out
}

/// `spec` with each `%define`/`%global` in `holders` whose value is its old tag
/// set to `tag`. The line keeps its indent, keyword, name and spacing.
fn retag_defines(spec: &str, holders: &[(String, String)], tag: &str) -> String {
    let lines: Vec<String> = spec
        .lines()
        .map(|line| {
            let t = line.trim_start();
            let indent = &line[..line.len() - t.len()];
            let Some(rest) = t
                .strip_prefix("%define ")
                .or_else(|| t.strip_prefix("%global "))
                .or_else(|| t.strip_prefix("%define\t"))
                .or_else(|| t.strip_prefix("%global\t"))
            else {
                return line.to_string();
            };
            let keyword = &t[..t.len() - rest.len()];
            let Some(name_end) = rest.find([' ', '\t']) else {
                return line.to_string();
            };
            let (name, after) = rest.split_at(name_end);
            let value = after.trim_start();
            let gap = &after[..after.len() - value.len()];
            match holders.iter().find(|(n, _)| n == name) {
                Some((_, old_tag)) if value.trim_end() == old_tag => {
                    format!("{indent}{keyword}{name}{gap}{tag}")
                }
                _ => line.to_string(),
            }
        })
        .collect();
    lines.join("\n") + if spec.ends_with('\n') { "\n" } else { "" }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The lines of `louvre.spec` that carry the tag (the rest is elided).
    const LOUVRE: &str = "\
Name:           liblouvre
Version:        1.0.0.1
%define sillyver 1.0.0-1
%define lname liblouvre
Source:         https://github.com/CuarzoSoftware/Louvre/archive/refs/tags/v%sillyver.tar.gz
%autosetup -p1 -n Louvre-%sillyver
";

    const SRM: &str = "\
Name:           cuarzo-srm
Version:        0.4.0.1
%define sillyver 0.4.0-1
%define lname\tlibSRM0
Source:         https://github.com/CuarzoSoftware/SRM/archive/refs/tags/v%sillyver.tar.gz
%autosetup -p1 -n SRM-%sillyver
";

    #[test]
    fn a_tag_define_is_updated_with_the_version() {
        let out = set_version_and_tag(LOUVRE, "1.0.2.1", Some("1.0.2-1")).unwrap();
        assert_eq!(
            out,
            LOUVRE
                .replace("Version:        1.0.0.1", "Version:        1.0.2.1")
                .replace("sillyver 1.0.0-1", "sillyver 1.0.2-1")
        );
        let out = set_version_and_tag(SRM, "0.4.1.2", Some("0.4.1-2")).unwrap();
        assert!(out.contains("%define sillyver 0.4.1-2\n"), "{out}");
        assert!(out.contains("%define lname\tlibSRM0\n"), "{out}");
        assert!(out.contains("Source:         https://github.com/CuarzoSoftware/SRM/archive/refs/tags/v%sillyver.tar.gz"));
        assert!(out.contains("%autosetup -p1 -n SRM-%sillyver"));
    }

    #[test]
    fn a_plain_version_without_a_tag_define_is_set_as_before() {
        let spec = "Name: x\nVersion: 1.0\nSource: x-%{version}.tar.gz\n";
        assert_eq!(
            set_version_and_tag(spec, "2.0", None),
            set_version(spec, "2.0").ok_or_else(String::new)
        );
    }

    #[test]
    fn a_plain_tag_for_a_tag_define_is_written_plain() {
        let out = set_version_and_tag(LOUVRE, "2.0", Some("2.0")).unwrap();
        assert!(out.contains("%define sillyver 2.0\n"), "{out}");
        assert!(out.contains("Version:        2.0\n"), "{out}");
        // A package going back to plain tags has no tag to pass: the version is it.
        let out = set_version_and_tag(LOUVRE, "2.0", None).unwrap();
        assert!(out.contains("%define sillyver 2.0\n"), "{out}");
        assert!(out.contains("Version:        2.0\n"), "{out}");
    }

    /// A define that holds a tag with its `v` keeps the `v`: the `Source:` names
    /// the tag as the define spells it.
    #[test]
    fn a_tag_define_keeps_the_prefix_of_the_tag() {
        let spec = LOUVRE
            .replace("sillyver 1.0.0-1", "sillyver v1.0.0-1")
            .replace("tags/v%sillyver", "tags/%sillyver");
        let out = set_version_and_tag(&spec, "1.0.2.1", Some("v1.0.2-1")).unwrap();
        assert!(out.contains("%define sillyver v1.0.2-1\n"), "{out}");
        assert!(
            out.contains("tags/%sillyver.tar.gz") && out.contains("-n Louvre-%sillyver"),
            "{out}"
        );
        assert!(out.contains("Version:        1.0.2.1\n"), "{out}");
    }

    /// `Version: %{pkgver}` is the packaged tag's version through its define, so
    /// the tag define is found and the version define is set.
    #[test]
    fn a_version_macro_is_resolved_to_the_packaged_tag() {
        let spec = "%define pkgver 1.0.0.1\n%define sillyver 1.0.0-1\nName: liblouvre\n\
                    Version: %{pkgver}\n\
                    Source: https://github.com/CuarzoSoftware/Louvre/archive/refs/tags/%sillyver.tar.gz\n";
        let out = set_version_and_tag(spec, "1.0.2.1", Some("1.0.2-1")).unwrap();
        assert!(out.contains("%define sillyver 1.0.2-1\n"), "{out}");
        assert!(out.contains("%define pkgver 1.0.2.1\n"), "{out}");
    }

    #[test]
    fn a_dashed_tag_without_a_define_gets_an_upstream_version_define() {
        let spec = "Name: liblouvre\nVersion: 1.0.0.1\n\
                    Source: https://github.com/CuarzoSoftware/Louvre/archive/refs/tags/v%{version}.tar.gz\n\
                    %autosetup -p1 -n Louvre-%{version}\n%files\n%{_libdir}/lib%{version}\n";
        let out = set_version_and_tag(spec, "1.0.1.1", Some("1.0.1-1")).unwrap();
        assert_eq!(
            out,
            "Name: liblouvre\nVersion: 1.0.1.1\n%define upstream_version 1.0.1-1\n\
             Source: https://github.com/CuarzoSoftware/Louvre/archive/refs/tags/v%{upstream_version}.tar.gz\n\
             %autosetup -p1 -n Louvre-%{upstream_version}\n%files\n%{_libdir}/lib%{version}\n"
        );
        // Nothing names the version: a maintainer's job.
        let fixed = "Name: x\nVersion: 1.0.0.1\nSource: https://example.org/x.tar.gz\n%autosetup\n";
        assert!(set_version_and_tag(fixed, "1.0.1.1", Some("1.0.1-1")).is_err());
        // A define that already holds the old tag is updated, not added again.
        let defined = spec.replace(
            "Version: 1.0.0.1",
            "Version: 1.0.0.1\n%define upstream_version 1.0.0-1",
        );
        let out = set_version_and_tag(&defined, "1.0.1.1", Some("1.0.1-1")).unwrap();
        assert!(out.contains("%define upstream_version 1.0.1-1\n"), "{out}");
        assert_eq!(out.matches("%define upstream_version").count(), 1, "{out}");
        // Some other define of that name is not guessed at.
        let other = spec.replace(
            "Version: 1.0.0.1",
            "Version: 1.0.0.1\n%define upstream_version 2.0",
        );
        assert!(set_version_and_tag(&other, "1.0.1.1", Some("1.0.1-1")).is_err());
    }

    /// `%version` is the same macro: only the tag archive and the `-n` directory
    /// change, and a subpackage's `Version:` gets no second define.
    #[test]
    fn the_define_goes_once_and_only_the_tarball_names_change() {
        let spec = "Name: liblouvre\nVersion: 1.0.0.1\nRelease: 0\n\
                    Source: https://github.com/CuarzoSoftware/Louvre/archive/refs/tags/v%version.tar.gz\n\
                    %autosetup -p1 -n Louvre-%version\n\
                    %package devel\nSummary: x\nVersion: 1.0.0.1\n%description devel\n%{version}\n";
        let out = set_version_and_tag(spec, "1.0.1.1", Some("1.0.1-1")).unwrap();
        assert_eq!(out.matches("%define upstream_version").count(), 1, "{out}");
        assert!(
            out.contains("Version: 1.0.1.1\n%define upstream_version 1.0.1-1\nRelease: 0\n"),
            "{out}"
        );
        assert!(out.contains("tags/v%{upstream_version}.tar.gz"), "{out}");
        assert!(out.contains("-n Louvre-%{upstream_version}\n"), "{out}");
        assert!(
            out.contains("Version: 1.0.0.1\n%description devel\n%{version}\n"),
            "{out}"
        );
    }

    /// The shapes this cannot edit are refused, each for its own reason.
    #[test]
    fn a_spec_of_another_shape_is_refused() {
        let base = "Name: liblouvre\nVersion: 1.0.0.1\n\
                    Source: https://github.com/CuarzoSoftware/Louvre/archive/refs/tags/v%{version}.tar.gz\n\
                    %autosetup -p1 -n Louvre-%{version}\n";
        // A vendored tarball named after the version would be left pointing at
        // a `%{upstream_version}` file that does not exist.
        let vendor = format!("{base}Source1: https://example.org/vendor-%{{version}}.tar.gz\n");
        // A second forge archive: which one is the release is not known.
        let two = format!(
            "{base}Source1: https://github.com/o/y/archive/refs/tags/v%{{version}}.tar.gz\n"
        );
        // A `Version:` in a conditional has no top-level place for the define.
        let cond = "Name: liblouvre\n%if 0%{?fedora}\nVersion: 1.0.0.1\n%endif\n\
                    Source: https://github.com/CuarzoSoftware/Louvre/archive/refs/tags/v%{version}.tar.gz\n\
                    %autosetup -p1 -n Louvre-%{version}\n";
        // Without a `-n` that names the version, the directory is the default one.
        let no_n = "Name: x\nVersion: 1.0.0.1\n\
                    Source: https://github.com/o/x/archive/refs/tags/v%{version}.tar.gz\n%autosetup\n";
        for spec in [vendor.as_str(), two.as_str(), cond, no_n] {
            let err = set_version_and_tag(spec, "1.0.1.1", Some("1.0.1-1")).unwrap_err();
            assert!(err.contains("needs a maintainer"), "{err}");
        }
    }

    #[test]
    fn a_bump_that_cannot_keep_the_tag_is_refused() {
        // The tag does not mean the version asked for.
        assert!(set_version_and_tag(LOUVRE, "1.0.2.1", Some("1.0.3-1")).is_err());
        // A dashed tag, no define holding the old one, and a forge archive that
        // names the version: the define is added (see the test above).
        let spec = "Name: x\nVersion: 1.0.0.1\n\
                    Source: https://github.com/o/x/archive/refs/tags/v%{version}.tar.gz\n\
                    %autosetup -n x-%{version}\n";
        assert!(set_version_and_tag(spec, "1.0.2.1", Some("1.0.2-1")).is_ok());
        // A plain source that names the version is not a tag archive.
        let plain =
            "Name: x\nVersion: 1.0.0.1\nSource: x-%{version}.tar.gz\n%autosetup -n x-%{version}\n";
        assert!(set_version_and_tag(plain, "1.0.2.1", Some("1.0.2-1")).is_err());
    }
}
