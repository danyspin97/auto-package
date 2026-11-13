use super::changes_normalise::normalise_unpublished_entries;
use super::changes_normalise_tests::{HEADER, entry};

#[test]
fn an_older_unpublished_entry_is_kept_below_the_newer_one() {
    // The 5.3 entry is older than the spec's 5.4: it keeps its own release,
    // header and recap, under the 5.4 entry, which stays on top. Its placeholder
    // line goes, which is the one change.
    let newer = entry(
        "Wed Nov  4 13:08:52 UTC 2026 - A <a@b.c>",
        "- Update to 5.4\n- Refresh n.patch",
    );
    let older = entry(
        HEADER,
        "- Update to 5.3\n  * No upstream changelog found or provided\n  * old recap\n- Refresh o.patch",
    );
    let branch = format!("{newer}{older}");
    let out = normalise_unpublished_entries("", &branch, "5.4", "pkg").unwrap();
    assert!(
        out.contains(&format!(
            "{HEADER}\n\n- Update to 5.3\n  * old recap\n- Refresh o.patch\n"
        )),
        "{out}"
    );
    assert!(
        out.find("Update to 5.4") < out.find("Update to 5.3"),
        "{out}"
    );
    assert_eq!(out.matches("Update to 5.3").count(), 1, "{out}");
    assert_eq!(normalise_unpublished_entries("", &out, "5.4", "pkg"), None);
}

#[test]
fn a_newer_unpublished_entry_below_the_spec_is_dropped_and_an_older_one_kept() {
    // Spec 5.3: the 5.4 entry is an abandoned attempt past it and goes; the
    // 5.2 entry is older and stays.
    let spec = entry(
        "Wed Nov  4 13:08:52 UTC 2026 - A <a@b.c>",
        "- Update to 5.3\n- Refresh s.patch",
    );
    let newer = entry(HEADER, "- Update to 5.4\n- Refresh n.patch");
    let older = entry(HEADER, "- Update to 5.2\n- Refresh o.patch");
    let branch = format!("{spec}{newer}{older}");
    let out = normalise_unpublished_entries("", &branch, "5.3", "pkg").unwrap();
    assert!(!out.contains("5.4"), "{out}");
    assert!(!out.contains("Refresh n.patch"), "{out}");
    assert!(
        out.contains("- Update to 5.2\n- Refresh o.patch\n"),
        "{out}"
    );
    assert!(out.contains("Refresh s.patch"), "{out}");
}

#[test]
fn an_entry_that_cannot_be_ordered_against_the_spec_is_dropped() {
    // `5.3/x` names a version that starts like one but cannot be ordered
    // against 5.4, so it is dropped as before.
    let newer = entry(
        "Wed Nov  4 13:08:52 UTC 2026 - A <a@b.c>",
        "- Update to 5.4\n- Refresh n.patch",
    );
    let odd = entry(HEADER, "- Update to 5.3/x\n- Refresh o.patch");
    let out = normalise_unpublished_entries("", &format!("{newer}{odd}"), "5.4", "pkg").unwrap();
    assert!(!out.contains("5.3/x"), "{out}");
    assert!(!out.contains("Refresh o.patch"), "{out}");
    assert!(out.contains("Refresh n.patch"), "{out}");
}

/// The origin of pyenv: 2.8.4 is published, 2.8.14 and 2.8.15 are not.
const PYENV_ORIGIN: &str = "\
-------------------------------------------------------------------
Mon Sep 28 12:52:34 UTC 2026 - Andrea Manzini <andrea.manzini@suse.com>

- Update to 2.8.4:
  * Fix the shims.
- Add BuildRequires on gcc.
";

#[test]
fn the_pyenv_branch_keeps_the_unpublished_2_8_14_and_2_8_15_entries() {
    // The branch: the 2.8.15 entry on top, the 2.8.14 one it was made from,
    // then the origin's 2.8.4 entry. The spec is 2.8.15.
    let branch = format!(
        "{}{}{}",
        entry(
            "Wed Nov 11 09:00:00 UTC 2026 - A <a@b.c>",
            "- Update to 2.8.15:\n  * Fix the build on 3.14.\n- Refresh x.patch",
        ),
        entry(
            "Tue Nov 10 09:00:00 UTC 2026 - A <a@b.c>",
            "- Update to 2.8.14:\n  * No upstream changelog found or provided\n  * Add a new command.\n- Refresh y.patch",
        ),
        PYENV_ORIGIN_ENTRY,
    );
    let out = normalise_unpublished_entries(PYENV_ORIGIN, &branch, "2.8.15", "pyenv").unwrap();
    assert_eq!(out.matches("Update to 2.8.15").count(), 1, "{out}");
    assert_eq!(out.matches("Update to 2.8.14").count(), 1, "{out}");
    assert!(out.contains("* Add a new command."), "{out}");
    assert!(out.contains("* Fix the build on 3.14."), "{out}");
    assert!(
        out.find("Update to 2.8.15") < out.find("Update to 2.8.14"),
        "{out}"
    );
    assert!(out.ends_with(PYENV_ORIGIN), "{out}");
    assert_eq!(
        normalise_unpublished_entries(PYENV_ORIGIN, &out, "2.8.15", "pyenv"),
        None
    );
}

/// The origin's own 2.8.4 entry, as it is written at the end of the branch.
const PYENV_ORIGIN_ENTRY: &str = "\
-------------------------------------------------------------------
Mon Sep 28 12:52:34 UTC 2026 - Andrea Manzini <andrea.manzini@suse.com>

- Update to 2.8.4:
  * Fix the shims.
- Add BuildRequires on gcc.
";

const NEWER_HEADER: &str = "Wed Nov  4 13:08:52 UTC 2026 - A <a@b.c>";

#[test]
fn the_older_recap_bullets_of_one_entry_are_kept_in_order_and_idempotent() {
    let branch = entry(
        NEWER_HEADER,
        "- Update to 1.14.1:\n  * new\n- Update to 1.14.0:\n  * old\n- Refresh x.patch",
    );
    let out = normalise_unpublished_entries("", &branch, "1.14.1", "pkg").unwrap_or(branch);
    assert!(
        out.contains(
            "- Update to 1.14.1:\n  * new\n- Update to 1.14.0:\n  * old\n- Refresh x.patch"
        ),
        "{out}"
    );
    assert_eq!(
        normalise_unpublished_entries("", &out, "1.14.1", "pkg"),
        None
    );
}

#[test]
fn a_newer_recap_bullet_in_an_entry_is_dropped() {
    let branch = entry(
        NEWER_HEADER,
        "- Update to 1.14.1:\n  * new\n- Update to 1.15.0:\n  * stray\n- Update to 1.14.0:\n  * old",
    );
    let out = normalise_unpublished_entries("", &branch, "1.14.1", "pkg").unwrap();
    assert!(!out.contains("1.15.0"), "{out}");
    assert!(!out.contains("stray"), "{out}");
    assert!(out.contains("- Update to 1.14.0:\n  * old"), "{out}");
}

#[test]
fn an_older_recap_the_origin_publishes_is_dropped() {
    let origin = entry(HEADER, "- Update to 1.14.0:\n  * published");
    let branch = entry(
        NEWER_HEADER,
        "- Update to 1.14.1:\n  * new\n- Update to 1.14.0:\n  * again",
    );
    let out = normalise_unpublished_entries(&origin, &branch, "1.14.1", "pkg").unwrap();
    assert!(out.contains("- Update to 1.14.1:\n  * new"), "{out}");
    assert!(!out.contains("again"), "{out}");
}

#[test]
fn the_older_recap_of_a_merged_entry_is_kept_once_below_the_release() {
    let newest = entry(NEWER_HEADER, "- Update to 1.14.1:\n  * new");
    let older = entry(
        HEADER,
        "- Update to 1.14.1:\n  * other\n- Update to 1.14.0:\n  * old",
    );
    let branch = format!("{newest}{older}");
    let out = normalise_unpublished_entries("", &branch, "1.14.1", "pkg").unwrap();
    assert_eq!(out.matches("Update to 1.14.0").count(), 1, "{out}");
    assert!(
        out.contains("- Update to 1.14.1:\n  * new\n- Update to 1.14.0:\n  * old"),
        "{out}"
    );
    assert_eq!(
        normalise_unpublished_entries("", &out, "1.14.1", "pkg"),
        None
    );
}
