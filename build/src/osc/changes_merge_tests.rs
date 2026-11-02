use crate::osc::changes_merge::rewrite_entry_from_diff;
use std::collections::BTreeSet;

/// The duckdb 2.0.0 entry of run 1 as the agent turns left it (server:database
/// branch, `duckdb.changes`): nine refreshes, then one bullet per agent fix.
const DUCKDB_NEW: &str = "\
-------------------------------------------------------------------
Mon Nov  2 16:04:50 UTC 2026 - Danilo Spinella <danilo.spinella@suse.com>

- Update to 2.0.0
  * Too many changes to list, see:
    https://github.com/duckdb/duckdb/releases/tag/v2.0.0
- Refresh duckdb-parquet-aad-little-endian.patch
- Refresh duckdb-parquet-footer-little-endian.patch
- Refresh duckdb-parquet-level-length-little-endian.patch
- Refresh duckdb-parquet-primitives-little-endian.patch
- Refresh duckdb-shared-only-cmake-export.patch
- Refresh duckdb-use-system-additional-libraries.patch
- Refresh duckdb-use-system-parquet-codecs.patch
- Refresh duckdb-use-system-yyjson.patch
- Refresh duckdb-use-system-zstd.patch
- Rename the shared library subpackage to libduckdb2_0 for the new
  soname 2.0
- Update the libduckdb.so file list to the 2.0 soname
- Update duckdb-use-system-yyjson.patch to ship a yyjson_memory.hpp
  shim, as the sources now include it
- Fix duckdb-use-system-yyjson.patch to drop the removed fast_mem
  include from the yyjson shim
- Update duckdb-use-system-additional-libraries.patch to drop the
  duckdb_apache namespace alias
- Map duckdb_apache onto the system thrift namespace with a
  compiler define
- Extend duckdb-use-system-zstd.patch to build the ICU extension
  against system zstd

";

/// The entry before it, which OBS holds (`obs-baseline/files/duckdb.changes`).
const DUCKDB_OLD: &str = "\
-------------------------------------------------------------------
Mon Oct 19 17:05:01 UTC 2026 - jason woods <gnujason@outlook.com>

- Add duckdb-parquet-aad-little-endian.patch to serialize Parquet
  row-group, column and page AAD ordinals in little-endian order,
  fixing Arrow encrypted-Parquet authentication failures on s390x.
- Add an exact-byte regression test with non-symmetric ordinals and
  run it in %check; retain Arrow encryption interoperability coverage.
- Leave the shared AdditionalAuthenticatedData::WriteData unchanged.

";

/// The duckdb 2.0.0 package files: what OBS holds plus the new archive.
fn duckdb_files() -> BTreeSet<String> {
    [
        "duckdb-2.0.0.tar.gz",
        "duckdb-parquet-aad-little-endian.patch",
        "duckdb-parquet-footer-little-endian.patch",
        "duckdb-parquet-level-length-little-endian.patch",
        "duckdb-parquet-primitives-little-endian.patch",
        "duckdb-rpmlintrc.in",
        "duckdb-shared-only-cmake-export.patch",
        "duckdb-use-system-additional-libraries.patch",
        "duckdb-use-system-parquet-codecs.patch",
        "duckdb-use-system-yyjson.patch",
        "duckdb-use-system-zstd.patch",
        "duckdb.changes",
        "duckdb.spec",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// Every file of the duckdb package changed by the run, the archive included.
fn duckdb_changed() -> BTreeSet<String> {
    let mut changed = duckdb_files();
    changed.retain(|n| n != "duckdb-rpmlintrc.in");
    changed.insert("duckdb-1.5.6.tar.gz".to_string());
    changed
}

fn set(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|s| s.to_string()).collect()
}

/// `DUCKDB_NEW` followed by `DUCKDB_OLD`: a `.changes` file with one unpublished entry.
fn duckdb_file() -> String {
    format!("{DUCKDB_NEW}{DUCKDB_OLD}")
}

#[test]
fn the_duckdb_bullets_merge_by_the_file_they_name() {
    let out = rewrite_entry_from_diff(
        &duckdb_file(),
        DUCKDB_OLD,
        &duckdb_changed(),
        &duckdb_files(),
    )
    .unwrap();
    let expected = format!(
        "\
-------------------------------------------------------------------
Mon Nov  2 16:04:50 UTC 2026 - Danilo Spinella <danilo.spinella@suse.com>

- Update to 2.0.0
  * Too many changes to list, see:
    https://github.com/duckdb/duckdb/releases/tag/v2.0.0
- Refresh duckdb-parquet-aad-little-endian.patch
- Refresh duckdb-parquet-footer-little-endian.patch
- Refresh duckdb-parquet-level-length-little-endian.patch
- Refresh duckdb-parquet-primitives-little-endian.patch
- Refresh duckdb-shared-only-cmake-export.patch
- Update duckdb-use-system-additional-libraries.patch to drop the
  duckdb_apache namespace alias
- Refresh duckdb-use-system-parquet-codecs.patch
- Update duckdb-use-system-yyjson.patch to ship a yyjson_memory.hpp
  shim, as the sources now include it
- Extend duckdb-use-system-zstd.patch to build the ICU extension
  against system zstd
- Rename the shared library subpackage to libduckdb2_0 for the new
  soname 2.0
- Update the libduckdb.so file list to the 2.0 soname
- Map duckdb_apache onto the system thrift namespace with a
  compiler define

{DUCKDB_OLD}"
    );
    assert_eq!(out, expected);
    // The first bullet of each group is the one kept, not a later refinement.
    assert!(!out.contains("fast_mem"), "{out}");
    assert!(!out.contains("Rework"), "{out}");
}

#[test]
fn a_group_keeps_its_first_bullet_text_not_a_later_refinement() {
    let out = rewrite_entry_from_diff(
        &duckdb_file(),
        DUCKDB_OLD,
        &duckdb_changed(),
        &duckdb_files(),
    )
    .unwrap();
    assert!(
        out.contains(
            "- Update duckdb-use-system-yyjson.patch to ship a yyjson_memory.hpp\n  shim, as the sources now include it\n"
        ),
        "{out}"
    );
    assert!(
        out.contains(
            "- Update duckdb-use-system-additional-libraries.patch to drop the\n  duckdb_apache namespace alias\n"
        ),
        "{out}"
    );
}

#[test]
fn a_spec_only_bullet_stays_as_written() {
    let text = "\
-------------------------------------------------------------------
Mon Nov  2 16:04:50 UTC 2026 - Danilo Spinella <danilo.spinella@suse.com>

- Update to 2.0.0
- Set the CXXFLAGS of duckdb.spec to build with -O2
  and without the system zstd headers
- Add a yyjson option to duckdb-use-system-yyjson.patch

";
    // Only the yyjson bullet goes; the spec bullet is kept as written.
    let mut changed = duckdb_changed();
    changed.remove("duckdb-use-system-yyjson.patch");
    let out = rewrite_entry_from_diff(text, "", &changed, &duckdb_files()).unwrap();
    assert!(!out.contains("yyjson"), "{out}");
    assert!(
        out.contains(
            "- Set the CXXFLAGS of duckdb.spec to build with -O2\n  and without the system zstd headers\n"
        ),
        "{out}"
    );
}

#[test]
fn a_later_revert_replace_instead_undo_or_drop_replaces_the_first_bullet() {
    for word in ["Revert", "Replace", "Instead", "Undo", "Drop"] {
        let text = format!(
            "\
-------------------------------------------------------------------
Mon Nov  2 16:04:50 UTC 2026 - Danilo Spinella <danilo.spinella@suse.com>

- Update to 2.0.0
- Add the zstd build option to duckdb-use-system-zstd.patch
- Refresh duckdb-use-system-zstd.patch
- {word} the zstd build option in duckdb-use-system-zstd.patch

"
        );
        let out = rewrite_entry_from_diff(
            &text,
            "",
            &set(&["duckdb-use-system-zstd.patch"]),
            &duckdb_files(),
        )
        .unwrap();
        assert!(
            out.contains(&format!(
                "- {word} the zstd build option in duckdb-use-system-zstd.patch\n"
            )),
            "{out}"
        );
        assert!(!out.contains("Add the zstd"), "{out}");
    }
}

#[test]
fn the_last_replacing_bullet_wins_over_an_earlier_one() {
    let text = "\
-------------------------------------------------------------------
Mon Nov  2 16:04:50 UTC 2026 - Danilo Spinella <danilo.spinella@suse.com>

- Update to 2.0.0
- Add the zstd build option to duckdb-use-system-zstd.patch
- Undo the zstd build option in duckdb-use-system-zstd.patch
- Fix the zstd build option in duckdb-use-system-zstd.patch
- Drop the zstd build option in duckdb-use-system-zstd.patch

";
    let out = rewrite_entry_from_diff(
        text,
        "",
        &set(&["duckdb-use-system-zstd.patch"]),
        &duckdb_files(),
    )
    .unwrap();
    assert!(
        out.contains("- Drop the zstd build option in duckdb-use-system-zstd.patch\n"),
        "{out}"
    );
    assert!(!out.contains("Undo") && !out.contains("Fix the"), "{out}");
}

#[test]
fn a_first_replacing_bullet_is_kept_when_no_later_one_replaces_it() {
    let text = "\
-------------------------------------------------------------------
Mon Nov  2 16:04:50 UTC 2026 - Danilo Spinella <danilo.spinella@suse.com>

- Update to 2.0.0
- Revert the zstd build option in duckdb-use-system-zstd.patch
- Fix the zstd build option in duckdb-use-system-zstd.patch

";
    let out = rewrite_entry_from_diff(
        text,
        "",
        &set(&["duckdb-use-system-zstd.patch"]),
        &duckdb_files(),
    )
    .unwrap();
    assert!(
        out.contains("- Revert the zstd build option in duckdb-use-system-zstd.patch\n"),
        "{out}"
    );
    assert!(!out.contains("Fix the"), "{out}");
}

#[test]
fn a_refresh_only_file_keeps_its_refresh_bullet() {
    let text = "\
-------------------------------------------------------------------
Mon Nov  2 16:04:50 UTC 2026 - Danilo Spinella <danilo.spinella@suse.com>

- Update to 2.0.0
- Refresh duckdb-use-system-zstd.patch
- Refresh duckdb-use-system-yyjson.patch
- Extend duckdb-use-system-yyjson.patch to build the ICU extension

";
    let out = rewrite_entry_from_diff(
        text,
        "",
        &set(&[
            "duckdb-use-system-zstd.patch",
            "duckdb-use-system-yyjson.patch",
        ]),
        &duckdb_files(),
    )
    .unwrap();
    // zstd only changed by its refresh: the refresh bullet is kept as it is.
    assert!(
        out.contains("- Refresh duckdb-use-system-zstd.patch\n"),
        "{out}"
    );
    // yyjson has a real change: its refresh is superseded by the change.
    assert!(
        !out.contains("Refresh duckdb-use-system-yyjson.patch"),
        "{out}"
    );
    assert!(
        out.contains("- Extend duckdb-use-system-yyjson.patch to build the ICU extension\n"),
        "{out}"
    );
}

#[test]
fn a_bullet_naming_only_files_the_diff_leaves_alone_is_dropped() {
    let mut changed = duckdb_changed();
    changed.remove("duckdb-use-system-yyjson.patch");
    let out =
        rewrite_entry_from_diff(&duckdb_file(), DUCKDB_OLD, &changed, &duckdb_files()).unwrap();
    // The three yyjson bullets name a patch the final diff does not change.
    assert!(!out.contains("yyjson"), "{out}");
    // The other files keep their merged bullets, and the version bullet stays.
    assert!(out.contains("- Update to 2.0.0\n"), "{out}");
    assert!(
        out.contains("- Extend duckdb-use-system-zstd.patch"),
        "{out}"
    );
}

#[test]
fn an_entry_the_published_file_already_has_is_left_alone() {
    let published = duckdb_file();
    assert!(
        rewrite_entry_from_diff(
            &duckdb_file(),
            &published,
            &duckdb_changed(),
            &duckdb_files()
        )
        .is_none()
    );
}

#[test]
fn an_entry_without_an_update_bullet_is_left_alone() {
    let text = "\
-------------------------------------------------------------------
Mon Nov  2 16:04:50 UTC 2026 - Danilo Spinella <danilo.spinella@suse.com>

- Refresh duckdb-use-system-zstd.patch
- Extend duckdb-use-system-zstd.patch to build the ICU extension

";
    assert!(
        rewrite_entry_from_diff(text, "", &duckdb_changed(), &duckdb_files()).is_none(),
        "no version to name the merged bullet by"
    );
}

#[test]
fn a_file_with_one_bullet_per_file_is_not_rewritten() {
    let text = "\
-------------------------------------------------------------------
Mon Nov  2 16:04:50 UTC 2026 - Danilo Spinella <danilo.spinella@suse.com>

- Update to 2.0.0
- Refresh duckdb-use-system-zstd.patch

";
    assert!(rewrite_entry_from_diff(text, "", &duckdb_changed(), &duckdb_files()).is_none());
}

#[test]
fn an_update_bullet_stays_even_when_a_line_under_it_names_an_unchanged_file() {
    let text = "\
-------------------------------------------------------------------
Mon Nov  2 16:04:50 UTC 2026 - Danilo Spinella <danilo.spinella@suse.com>

- Update to 2.0.0
  FLAG: duckdb-use-system-yyjson.patch unchanged
- Refresh duckdb-use-system-zstd.patch
- Extend duckdb-use-system-zstd.patch to build the ICU extension

";
    let out = rewrite_entry_from_diff(
        text,
        "",
        &set(&["duckdb-use-system-zstd.patch"]),
        &duckdb_files(),
    )
    .unwrap();
    assert!(
        out.contains("- Update to 2.0.0\n  FLAG: duckdb-use-system-yyjson.patch unchanged\n"),
        "{out}"
    );
    assert!(
        out.contains("- Extend duckdb-use-system-zstd.patch"),
        "{out}"
    );
}

#[test]
fn a_bullet_naming_several_files_joins_the_first_one_and_drops_when_none_changed() {
    let text = "\
-------------------------------------------------------------------
Mon Nov  2 16:04:50 UTC 2026 - Danilo Spinella <danilo.spinella@suse.com>

- Update to 2.0.0
- Refresh duckdb-use-system-zstd.patch
- Share duckdb-use-system-zstd.patch and duckdb-use-system-yyjson.patch options

";
    // Only zstd changed: the shared bullet joins zstd's group, not yyjson's.
    let out = rewrite_entry_from_diff(
        text,
        "",
        &set(&["duckdb-use-system-zstd.patch"]),
        &duckdb_files(),
    )
    .unwrap();
    assert!(
        out.contains(
            "- Share duckdb-use-system-zstd.patch and duckdb-use-system-yyjson.patch options"
        ),
        "{out}"
    );
    // Neither changed: the bullet is dropped with its file.
    let none = rewrite_entry_from_diff(text, "", &set(&[]), &duckdb_files()).unwrap();
    assert!(!none.contains("zstd"), "{none}");
}

#[test]
fn a_second_pass_over_the_result_changes_nothing() {
    let once = rewrite_entry_from_diff(
        &duckdb_file(),
        DUCKDB_OLD,
        &duckdb_changed(),
        &duckdb_files(),
    )
    .unwrap();
    assert!(
        rewrite_entry_from_diff(&once, DUCKDB_OLD, &duckdb_changed(), &duckdb_files()).is_none()
    );
}
