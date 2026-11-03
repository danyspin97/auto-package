//! Tests for the tool grants of a PatchReview turn.

use super::*;

fn grants(patches: &[&str]) -> Vec<String> {
    let patches: Vec<String> = patches.iter().map(|p| p.to_string()).collect();
    review_allowed_tools(
        std::path::Path::new("/srv/ws"),
        Some(std::path::Path::new("/srv/checkout")),
        &patches,
    )
    .split(',')
    .map(str::to_string)
    .collect()
}

#[test]
fn a_review_may_write_source_and_the_named_patch_only() {
    let tools = grants(&["a.patch"]);
    let writes: Vec<&String> = tools
        .iter()
        .filter(|t| t.starts_with("Edit(") || t.starts_with("Write("))
        .collect();
    assert_eq!(
        writes,
        [
            "Edit(//srv/ws/source/**)",
            "Write(//srv/ws/source/**)",
            "Edit(//srv/checkout/a.patch)",
            "Write(//srv/checkout/a.patch)",
        ]
    );
    assert!(tools.iter().any(|t| t == "Read(//srv/checkout/**)"));
    assert!(!tools.iter().any(|t| t.contains("b.patch")));
}

#[test]
fn a_review_cannot_create_delete_import_or_head_a_patch() {
    let tools = grants(&["a.patch"]).join(",");
    for sub in [
        "new", "delete", "import", "rename", "fold", "header", "snapshot", "revert",
    ] {
        assert!(!tools.contains(&format!("quilt {sub} ")), "{sub}: {tools}");
    }
    for sub in ["push", "pop", "refresh", "diff"] {
        assert!(
            tools.contains(&format!("Bash(cd source && quilt {sub} *)")),
            "{sub}"
        );
    }
    assert!(!tools.contains("patch *--dry-run"), "{tools}");
}

#[test]
fn a_review_is_denied_the_spec_the_changes_and_the_service() {
    let denied = review_denied_tools(Some(std::path::Path::new("/srv/checkout")));
    for file in ["*.spec", "*.changes", "_service"] {
        for tool in ["Edit", "Write", "NotebookEdit"] {
            let want = format!("{tool}(//srv/checkout/{file})");
            assert!(denied.contains(&want), "{want}");
        }
    }
    assert!(review_denied_tools(None).is_empty());
}

#[test]
fn a_review_gets_no_byte_edit_grant_and_no_addendum() {
    let tools = grants(&["a.patch"]).join(",");
    assert!(!tools.contains("byte-edit"), "{tools}");
    assert!(edit_bytes_addendum(Task::PatchReview).is_empty());
    assert!(!edit_bytes_addendum(Task::RefreshPatches).is_empty());
}

#[test]
fn a_hostile_patch_name_gets_no_grant() {
    for bad in [
        "*", "**", "a b", "a,b", "../x", "(x)", "[a]", ".hidden", "-p", "", "a/b",
    ] {
        assert!(!safe_review_patch_name(bad), "{bad:?}");
        let tools = grants(&[bad]);
        let writes = tools
            .iter()
            .filter(|t| t.starts_with("Edit(") || t.starts_with("Write("))
            .count();
        assert_eq!(writes, 2, "{bad:?}: only source/ is writable: {tools:?}");
    }
    for good in ["a.patch", "foo-1.0_fix+x.diff"] {
        assert!(safe_review_patch_name(good), "{good}");
    }
}
