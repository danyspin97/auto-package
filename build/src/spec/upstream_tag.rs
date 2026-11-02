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
            Some(tag) if tag != version => Err(format!(
                "the spec has no define holding the tag {tag} of the packaged version {old}; \
                 needs a maintainer"
            )),
            _ => plain(),
        };
    }
    // A plain tag (`None`) is the version itself: the define takes it as it is.
    let tag = tag.unwrap_or(version);
    let retagged = retag_defines(spec, &holders, tag);
    set_version(&retagged, version).ok_or_else(|| "its Version: is not a plain value".into())
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
    fn a_bump_that_cannot_keep_the_tag_is_refused() {
        // The tag does not mean the version asked for.
        assert!(set_version_and_tag(LOUVRE, "1.0.2.1", Some("1.0.3-1")).is_err());
        // A dashed tag and no define that holds the old one: no spec to edit.
        let spec = "Name: x\nVersion: 1.0.0.1\nSource: x-%{version}.tar.gz\n";
        let err = set_version_and_tag(spec, "1.0.2.1", Some("1.0.2-1")).unwrap_err();
        assert!(err.contains("needs a maintainer"), "{err}");
    }
}
