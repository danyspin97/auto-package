//! Whether a package's build flags leave a repository out. The branch copies
//! its origin's flags (see `osc::sync_build_flags`), so a repository the
//! origin disables is one the branch does not build either, and a capability
//! missing there is not a gap to report.
//!
//! The rule implemented here, from what the flags are documented to do and
//! not checked against the OBS source: among the `<enable>` and `<disable>`
//! rules of one `<build>` section that match the repository and architecture,
//! the most specific one decides (naming both repository and architecture
//! beats naming the repository alone, which beats naming the architecture
//! alone, which beats naming neither), and among equally specific rules the
//! later one decides. The package's flags are consulted before the
//! project's.

use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};

/// Whether the package meta `meta_xml` disables building `repository` for
/// `arch`, by its own `<build>` flags alone. `false` when no rule matches.
pub fn build_disabled(meta_xml: &str, repository: &str, arch: &str) -> bool {
    flag_decision(meta_xml, repository, arch).unwrap_or(false)
}

/// Whether building `repository` for `arch` is disabled, given the metas that
/// set flags, most specific first (the package's, then the project's): the
/// first one with a matching rule decides. `false` when none has one.
pub fn build_disabled_in(levels: &[&str], repository: &str, arch: &str) -> bool {
    levels
        .iter()
        .find_map(|meta| flag_decision(meta, repository, arch))
        .unwrap_or(false)
}

/// The decision of the winning rule in `meta_xml` for `repository` and
/// `arch`: `Some(true)` when it disables, `Some(false)` when it enables, and
/// `None` when no rule matches. Only the `<build>` flags count.
pub fn flag_decision(meta_xml: &str, repository: &str, arch: &str) -> Option<bool> {
    let mut reader = Reader::from_str(meta_xml);
    // Open elements, the root being 1; the flags of <build> are at depth 3.
    let mut depth = 0usize;
    let mut in_build = false;
    // The winning rule so far: its specificity, and whether it disables.
    let mut winner: Option<(u8, bool)> = None;
    loop {
        let (e, is_start) = match reader.read_event() {
            Ok(Event::Start(e)) => (e, true),
            Ok(Event::Empty(e)) => (e, false),
            Ok(Event::End(e)) => {
                if depth == 2 && e.name().as_ref() == "build" {
                    in_build = false;
                }
                depth = depth.saturating_sub(1);
                continue;
            }
            Ok(Event::Eof) | Err(_) => break,
            Ok(_) => continue,
        };
        let here = depth + 1;
        if here == 2 && e.name().as_ref() == "build" {
            in_build = true;
        }
        if in_build && here == 3 {
            if let Some((repo, arch_rule, disables)) = flag_rule(&e) {
                let repo_ok = repo.as_deref().is_none_or(|r| r == repository);
                let arch_ok = arch_rule.as_deref().is_none_or(|a| a == arch);
                if repo_ok && arch_ok {
                    let specificity = u8::from(repo.is_some()) * 2 + u8::from(arch_rule.is_some());
                    // A rule at least as specific as the winner so far takes over.
                    if winner.is_none_or(|(s, _)| specificity >= s) {
                        winner = Some((specificity, disables));
                    }
                }
            }
        }
        if is_start {
            depth = here;
        }
    }
    winner.map(|(_, disables)| disables)
}

/// The repository and architecture a `<disable>` or `<enable>` names (`None`
/// when it leaves one out), and whether it disables. `None` for any other
/// element.
fn flag_rule(e: &BytesStart) -> Option<(Option<String>, Option<String>, bool)> {
    let is_disable = match e.name().as_ref() {
        "disable" => true,
        "enable" => false,
        _ => return None,
    };
    let (mut repo, mut arch) = (None, None);
    for attr in e.attributes().flatten() {
        let value = attr
            .normalized_value(quick_xml::XmlVersion::Implicit1_0)
            .ok()
            .map(|v| v.into_owned());
        match attr.key.as_ref() {
            "repository" => repo = value,
            "arch" => arch = value,
            _ => {}
        }
    }
    Some((repo, arch, is_disable))
}

#[cfg(test)]
mod tests {
    use super::{build_disabled, build_disabled_in, flag_decision};

    const META: &str = r#"<package name="miriway" project="X">
  <title/>
  <build>
    <disable repository="16.0"/>
    <disable repository="16.1" arch="aarch64"/>
    <enable repository="16.0" arch="aarch64"/>
  </build>
  <publish><disable/></publish>
</package>"#;

    #[test]
    fn a_disabled_repository_and_arch_are_reported() {
        assert!(build_disabled(META, "16.0", "x86_64"));
        assert!(build_disabled(META, "16.1", "aarch64"));
    }

    #[test]
    fn a_later_enable_overrides_a_disable() {
        assert!(!build_disabled(META, "16.0", "aarch64"));
    }

    #[test]
    fn a_repository_not_named_is_built() {
        assert!(!build_disabled(META, "16.1", "x86_64"));
        assert!(!build_disabled("<package name=\"p\"/>", "16.1", "x86_64"));
    }

    #[test]
    fn publish_flags_do_not_count() {
        let meta = "<package name=\"p\"><publish><disable/></publish></package>";
        assert!(!build_disabled(meta, "16.1", "x86_64"));
    }

    #[test]
    fn a_disable_without_attributes_covers_every_repository() {
        let meta = "<package name=\"p\"><build><disable/></build></package>";
        assert!(build_disabled(meta, "16.1", "x86_64"));
    }

    fn build(rules: &str) -> String {
        format!("<package name=\"p\"><build>{rules}</build></package>")
    }

    #[test]
    fn the_most_specific_rule_decides_whatever_the_order() {
        // (rules, decision for 16.0 on x86_64); repo+arch > repo > arch > none.
        let cases = [
            ("<enable repository=\"16.0\"/><disable/>", Some(false)),
            ("<disable/><enable repository=\"16.0\"/>", Some(false)),
            (
                "<enable repository=\"16.0\" arch=\"x86_64\"/><disable repository=\"16.0\"/>",
                Some(false),
            ),
            (
                "<disable repository=\"16.0\" arch=\"x86_64\"/><enable repository=\"16.0\"/>",
                Some(true),
            ),
            (
                "<disable repository=\"16.0\"/><enable arch=\"x86_64\"/>",
                Some(true),
            ),
            ("<enable arch=\"x86_64\"/><disable/>", Some(false)),
            ("<disable/>", Some(true)),
            ("<disable repository=\"16.1\"/>", None),
        ];
        for (rules, want) in cases {
            assert_eq!(
                flag_decision(&build(rules), "16.0", "x86_64"),
                want,
                "{rules}"
            );
        }
    }

    #[test]
    fn among_equally_specific_rules_the_later_one_decides() {
        let meta = build("<disable repository=\"16.1\"/><enable repository=\"16.1\"/>");
        assert!(!build_disabled(&meta, "16.1", "x86_64"));
        let meta = build("<enable repository=\"16.1\"/><disable repository=\"16.1\"/>");
        assert!(build_disabled(&meta, "16.1", "x86_64"));
    }

    #[test]
    fn the_package_decides_before_the_project() {
        let package = build("<enable repository=\"16.0\"/>");
        let project = build("<disable/>");
        assert!(!build_disabled_in(&[&package, &project], "16.0", "x86_64"));
        // A package with no matching rule leaves the project's rule to decide.
        assert!(build_disabled_in(&[&build(""), &project], "16.0", "x86_64"));
        assert!(!build_disabled_in(
            &[&build(""), &build("")],
            "16.0",
            "x86_64"
        ));
    }
}
