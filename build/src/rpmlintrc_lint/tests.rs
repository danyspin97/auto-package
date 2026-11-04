use super::*;

/// fwupd-efi's file: the import, a blank line, a filter with its reason.
const FWUPD: &str =
    "from Config import *\n\n# reason: the EFI binary is not an ELF\naddFilter(\"no-binary\")\n";

#[test]
fn the_import_is_a_style_finding() {
    let found = audit("fwupd-efi-rpmlintrc", FWUPD);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].kind, Kind::RpmlintrcStyle);
    assert!(found[0].detail.contains("line 1"), "{found:?}");
}

#[test]
fn badness_other_lines_and_a_filter_without_a_reason_are_findings() {
    let text = "setBadness('x', 0)\naddFilter(\"a\")\n# reason: why\naddFilter(\"b\")\n";
    let found = audit("a-rpmlintrc", text);
    assert!(found.iter().all(|f| f.kind == Kind::RpmlintrcHygiene));
    let all: String = found.iter().map(|f| f.detail.clone()).collect();
    assert!(
        all.contains("`setBadness('x', 0)`) is not an `addFilter"),
        "{all}"
    );
    assert!(
        all.contains("`addFilter(\"a\")`) has no `# reason:"),
        "{all}"
    );
    assert_eq!(found.len(), 2, "{found:?}");
    assert!(crate::quality::needs_pipeline(&found));
}

#[test]
fn a_dot_rpmlintrc_name_is_only_reported() {
    let found = audit("fwupd-efi.rpmlintrc", "# reason: r\naddFilter(\"a\")\n");
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].kind, Kind::RpmlintrcName);
    assert!(found[0].detail.contains("fwupd-efi-rpmlintrc"));
    assert!(!crate::quality::needs_pipeline(&found));
}

#[test]
fn only_the_rpmlintrc_a_spec_names_is_read() {
    let spec = "Name: fwupd-efi\nVersion: 1.9\nSource0: x.tar.gz\nSource1: fwupd-efi-rpmlintrc\nSource2: https://example.org/a.patch\n";
    assert_eq!(spec_rpmlintrc_files(spec), ["fwupd-efi-rpmlintrc"]);
    assert!(spec_rpmlintrc_files("Name: a\nVersion: 1\nSource0: a.tar.gz\n").is_empty());
}
