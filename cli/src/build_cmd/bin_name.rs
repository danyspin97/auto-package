use ap_build::osc::{FACTORY, FactoryName, FactoryNames};
use ap_build::sources::{Ecosystem, RepoProbe};

/// What decides a Rust repository's name besides its probe: whether a name is
/// already filed (in the project's listing or the state directory) and what
/// openSUSE:Factory has.
pub(super) struct NameLookups<'a> {
    pub(super) filed: &'a dyn Fn(&str) -> bool,
    pub(super) factory: &'a dyn FactoryNames,
    /// Off for a build that must keep the name it was given.
    pub(super) enabled: bool,
}

/// Program names too common to claim as a package name of their own.
const GENERIC: [&str; 33] = [
    "server", "cli", "app", "tool", "test", "main", "run", "bin", "daemon", "agent", "client",
    "gui", "core", "lib", "util", "utils", "demo", "example", "hello", "web", "api", "ctl", "cmd",
    "service", "worker", "node", "proxy", "manager", "monitor", "tools", "common", "base", "all",
];

/// Whether `name` is a `-` or `_` delimited token of `krate`.
fn is_token_of(name: &str, krate: &str) -> bool {
    krate
        .to_lowercase()
        .split(['-', '_'])
        .any(|t| t == name.to_lowercase())
}

/// The repository's name: the last path segment of its URL, without `.git`.
fn repo_name(url: &str) -> &str {
    let last = url.trim_end_matches('/').rsplit('/').next().unwrap_or(url);
    last.strip_suffix(".git").unwrap_or(last)
}

/// A program name worth a package name: three characters or more, not a
/// generic word, and either a word of the crate's name or the repository's
/// own name (`jay` in `jay-compositor`, or the repository `jay`).
fn is_distinctive(bin: &str, krate: &str, repo_url: &str) -> bool {
    bin.chars().count() >= 3
        && !GENERIC.contains(&bin.to_lowercase().as_str())
        && (is_token_of(bin, krate) || repo_name(repo_url).eq_ignore_ascii_case(bin))
}

/// Whether the state directory holds a package filed under `rpm`: a state
/// directory with a checkout of it (its spec) below `checkout_root`. A state
/// directory alone may be left over from an unrelated earlier package.
pub(super) fn state_holds_package(
    state_root: &std::path::Path,
    checkout_root: &std::path::Path,
    rpm: &str,
) -> bool {
    let Some(ws) = ap_build::workspace::Workspace::find_existing(state_root, &[rpm]) else {
        return false;
    };
    let obs = ap_build::pipeline::PkgState::load(&ws)
        .obs_name
        .unwrap_or_else(|| rpm.to_string());
    checkout_root
        .join(&obs)
        .join(format!("{obs}.spec"))
        .is_file()
}

/// The name a new Rust package takes in place of its crate name, and the line
/// that says why. `name` is `None` when the crate name stays.
pub(super) struct BinaryName {
    pub(super) name: Option<String>,
    pub(super) note: Option<String>,
}

/// A crate that installs one program, named like the repository asked for
/// (`jay-compositor` building `jay`), is filed under the program's name, as
/// distributions name packages, when that is safe:
///
/// - a package already filed under the crate name keeps it, and one already
///   filed under the requested name keeps that: nothing is renamed or orphaned;
/// - openSUSE:Factory must not have a package of that name;
/// - when Factory cannot be asked, the crate name stays.
///
/// `None` when the rule does not apply at all (not a single-crate Rust
/// repository, several programs, a program named otherwise, a generic or
/// short program name, a build that must keep its name).
pub(super) fn binary_name(
    requested: &str,
    repo_url: &str,
    probe: &RepoProbe,
    lookups: &NameLookups<'_>,
) -> Option<BinaryName> {
    if !lookups.enabled || probe.ecosystem != Ecosystem::Rust || probe.subprojects.len() > 1 {
        return None;
    }
    let krate = probe.project_name.as_deref()?;
    let [bin] = probe.bin_names.as_slice() else {
        return None;
    };
    let rpm = |n: &str| probe.ecosystem.rpm_name(n);
    if rpm(bin) != rpm(requested)
        || rpm(krate) == rpm(requested)
        || !is_distinctive(bin, krate, repo_url)
    {
        return None;
    }
    let keep = |note: String| BinaryName {
        name: None,
        note: Some(note),
    };
    if (lookups.filed)(&rpm(krate)) {
        return Some(keep(format!(
            "'{krate}' is already filed in the project: keeping that name, not '{bin}'"
        )));
    }
    if (lookups.filed)(&rpm(bin)) {
        return Some(BinaryName {
            name: Some(bin.clone()),
            note: Some(format!(
                "'{bin}' is already filed in the project: using it, not '{krate}'"
            )),
        });
    }
    Some(match lookups.factory.check(&rpm(bin)) {
        FactoryName::Free => BinaryName {
            name: Some(bin.clone()),
            note: Some(format!(
                "crate '{krate}' installs one program, '{bin}', and {FACTORY} has no \
                 package of that name: using '{bin}'"
            )),
        },
        FactoryName::Taken => keep(format!(
            "{FACTORY} already has a package '{bin}': using the crate name '{krate}'"
        )),
        FactoryName::Unknown(why) => keep(format!(
            "the {FACTORY} check for '{bin}' failed ({why}): using the crate name \
             '{krate}'; run again when OBS is reachable to name it '{bin}'"
        )),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    struct Fake(FactoryName, Cell<u32>);
    impl FactoryNames for Fake {
        fn check(&self, _: &str) -> FactoryName {
            self.1.set(self.1.get() + 1);
            self.0.clone()
        }
    }

    fn probe(krate: &str, bins: &[&str]) -> RepoProbe {
        RepoProbe {
            ecosystem: Ecosystem::Rust,
            project_name: Some(krate.into()),
            project_dir: None,
            bin_names: bins.iter().map(|b| b.to_string()).collect(),
            subprojects: Vec::new(),
            archive_url: "https://example.org/a.tar.gz".into(),
        }
    }

    fn run(
        requested: &str,
        p: &RepoProbe,
        filed: &[&str],
        factory: FactoryName,
    ) -> (Option<BinaryName>, u32) {
        let fake = Fake(factory, Cell::new(0));
        let filed = |n: &str| filed.contains(&n);
        let got = binary_name(
            requested,
            &format!("https://example.org/o/{requested}"),
            p,
            &NameLookups {
                filed: &filed,
                factory: &fake,
                enabled: true,
            },
        );
        (got, fake.1.get())
    }

    #[test]
    fn the_programs_name_wins_when_factory_has_none() {
        let (got, _) = run(
            "jay",
            &probe("jay-compositor", &["jay"]),
            &[],
            FactoryName::Free,
        );
        let got = got.unwrap();
        assert_eq!(got.name.as_deref(), Some("jay"));
        let note = got.note.unwrap();
        assert!(note.contains("one program") && note.contains("openSUSE:Factory"));
    }

    #[test]
    fn a_collision_falls_back_to_the_crate_name() {
        let (got, _) = run(
            "jay",
            &probe("jay-compositor", &["jay"]),
            &[],
            FactoryName::Taken,
        );
        let got = got.unwrap();
        assert_eq!(got.name, None);
        assert!(got.note.unwrap().contains("already has a package 'jay'"));
    }

    #[test]
    fn an_unreachable_factory_keeps_the_crate_name_and_says_so() {
        let down = FactoryName::Unknown("timed out".into());
        let (got, _) = run("jay", &probe("jay-compositor", &["jay"]), &[], down);
        let got = got.unwrap();
        assert_eq!(got.name, None);
        let note = got.note.unwrap();
        assert!(note.contains("failed (timed out)") && note.contains("jay-compositor"));
    }

    #[test]
    fn a_package_filed_under_the_crate_name_keeps_it_without_asking() {
        let p = probe("jay-compositor", &["jay"]);
        let (got, asked) = run("jay", &p, &["jay-compositor"], FactoryName::Free);
        assert_eq!(got.unwrap().name, None);
        assert_eq!(asked, 0);
    }

    #[test]
    fn a_package_filed_under_the_requested_name_keeps_it_without_asking() {
        let p = probe("jay-compositor", &["jay"]);
        let (got, asked) = run("jay", &p, &["jay"], FactoryName::Taken);
        assert_eq!(got.unwrap().name.as_deref(), Some("jay"));
        assert_eq!(asked, 0);
    }

    #[test]
    fn several_programs_or_another_program_name_leave_the_crate_name() {
        let both = probe("jay-compositor", &["jay", "jay-portal"]);
        assert!(run("jay", &both, &[], FactoryName::Free).0.is_none());
        let other = probe("jay-compositor", &["jayd"]);
        assert!(run("jay", &other, &[], FactoryName::Free).0.is_none());
        let none = probe("jay-compositor", &[]);
        assert!(run("jay", &none, &[], FactoryName::Free).0.is_none());
    }

    #[test]
    fn a_request_for_the_crate_name_is_left_alone() {
        let p = probe("jay-compositor", &["jay"]);
        assert!(
            run("jay-compositor", &p, &[], FactoryName::Free)
                .0
                .is_none()
        );
    }

    #[test]
    fn other_ecosystems_and_monorepos_are_left_alone() {
        let mut p = probe("jay-compositor", &["jay"]);
        p.ecosystem = Ecosystem::PlainC;
        assert!(run("jay", &p, &[], FactoryName::Free).0.is_none());
        let mut p = probe("jay-compositor", &["jay"]);
        p.subprojects = vec![("a".into(), "a".into()), ("b".into(), "b".into())];
        assert!(run("jay", &p, &[], FactoryName::Free).0.is_none());
    }

    #[test]
    fn names_compare_the_way_rpm_names_do() {
        let p = probe("Jay-Compositor", &["Jay"]);
        let (got, _) = run("JAY", &p, &[], FactoryName::Free);
        assert_eq!(got.unwrap().name.as_deref(), Some("Jay"));
        let p = probe("jay_compositor", &["jay_x"]);
        assert!(run("jay-x", &p, &[], FactoryName::Free).0.is_none());
    }

    fn named(requested: &str, url: &str, p: &RepoProbe, enabled: bool) -> Option<BinaryName> {
        let fake = Fake(FactoryName::Free, Cell::new(0));
        binary_name(
            requested,
            url,
            p,
            &NameLookups {
                filed: &|_| false,
                factory: &fake,
                enabled,
            },
        )
    }

    #[test]
    fn a_generic_or_short_program_name_is_never_claimed() {
        for bin in [
            "server", "cli", "app", "tool", "daemon", "client", "util", "api", "xy",
        ] {
            let p = probe(&format!("acme-{bin}"), &[bin]);
            assert!(run(bin, &p, &[], FactoryName::Free).0.is_none(), "{bin}");
        }
    }

    #[test]
    fn the_name_must_be_a_crate_word_or_the_repository_name() {
        let p = probe("acme-suite", &["frobnicate"]);
        let url = "https://example.org/o/frobnicate";
        // The repository is called like the program: fine.
        assert!(named("frobnicate", url, &p, true).is_some());
        // A request that is not the repository's name, and not in the crate.
        let url = "https://example.org/o/acme-suite.git";
        assert!(named("frobnicate", url, &p, true).is_none());
        // A word of the crate's name, whatever the repository is called.
        let p = probe("jay_compositor", &["jay-x"]);
        assert!(named("jay-x", url, &p, true).is_none());
        let p = probe("acme-frobnicate", &["frobnicate"]);
        assert!(named("frobnicate", url, &p, true).is_some());
    }

    #[test]
    fn a_branch_build_keeps_its_name() {
        let p = probe("jay-compositor", &["jay"]);
        assert!(named("jay", "https://example.org/o/jay", &p, false).is_none());
    }

    #[test]
    fn a_state_directory_alone_is_not_a_filed_package() {
        let dir = tempfile::tempdir().unwrap();
        let (state, checkouts) = (dir.path().join("state"), dir.path().join("co"));
        std::fs::create_dir_all(state.join("jay")).unwrap();
        assert!(!state_holds_package(&state, &checkouts, "jay"));
        std::fs::create_dir_all(checkouts.join("jay")).unwrap();
        assert!(!state_holds_package(&state, &checkouts, "jay"));
        std::fs::write(checkouts.join("jay/jay.spec"), "Name: jay\n").unwrap();
        assert!(state_holds_package(&state, &checkouts, "jay"));
        assert!(!state_holds_package(&state, &checkouts, "other"));
    }
}
