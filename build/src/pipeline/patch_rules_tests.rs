use super::patch_rules::*;
use super::patch_rules_spec::*;
use crate::pipeline::snapshot::CheckoutSnapshot;
use std::path::Path;

const HEADER: &str = "Subject: Fix the build with gcc 15\n\
Description: gcc 15 rejects the implicit int in foo.c, so the build stops.\n\
Upstream: https://github.com/example/foo/pull/12\n\
---\n";

const DIFF: &str =
    "--- foo-1.0/src/foo.c\n+++ foo-1.0/src/foo.c\n@@ -1 +1 @@\n-int x;\n+int x = 0;\n";

const BEFORE: &str = "Name: foo\nVersion: 1.0\nSource0: foo-1.0.tar.gz\n";

/// The spec `BEFORE` plus a commented `Patch0:` line for `file`.
fn spec_with_patch(file: &str) -> String {
    format!("{BEFORE}# PATCH-FIX-UPSTREAM {file} -- gcc 15 build\nPatch0: {file}\n")
}

/// A checkout with `spec` and the patch `file` holding `text`.
fn checkout(spec: &str, file: &str, text: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("foo.spec"), spec).unwrap();
    std::fs::write(dir.path().join(file), text).unwrap();
    dir
}

fn header_with_upstream(upstream: &str) -> String {
    HEADER.replace(
        "Upstream: https://github.com/example/foo/pull/12",
        &format!("Upstream: {upstream}"),
    )
}

#[test]
fn a_patch_declaration_the_edit_adds_is_listed_and_an_old_one_is_not() {
    let after = spec_with_patch("foo-fix.patch");
    let spec = Path::new("foo.spec");
    let added = added_patches(spec, BEFORE, &after, None);
    assert_eq!(added.len(), 1);
    assert_eq!(added[0].1, "foo-fix.patch");
    assert_eq!(added[0].2, "foo-fix.patch");
    assert!(added_patches(spec, &after, &after, None).is_empty());
}

#[test]
fn the_comment_above_a_patch_line_needs_the_type_one_tag() {
    assert!(is_patch_comment(
        "# PATCH-FIX-UPSTREAM foo.patch gh#a/b#1 -- why"
    ));
    assert!(is_patch_comment(
        "# PATCH-FEATURE-OPENSUSE foo.patch -- why"
    ));
    assert!(!is_patch_comment("# fix the build"));
    assert!(!is_patch_comment("# PATCH-FIX-SOMEWHERE foo.patch"));
    assert!(!is_patch_comment("PATCH-FIX-UPSTREAM foo.patch"));
}

#[test]
fn a_header_with_subject_description_and_upstream_passes() {
    assert!(header_problems(&format!("{HEADER}{DIFF}")).is_empty());
    let not_reported = HEADER.replace(
        "Upstream: https://github.com/example/foo/pull/12",
        "Upstream: not yet reported",
    );
    assert!(header_problems(&format!("{not_reported}{DIFF}")).is_empty());
}

#[test]
fn the_upstream_forms_of_convention_111_pass_and_others_do_not() {
    for ok in [
        "submitted https://github.com/example/foo/pull/12",
        "merged",
        "never",
        "no",
        "tbd (waiting for a release)",
        "https://github.com/example/foo/commit/abc1234 (fixed in 2.0)",
        "not yet reported",
    ] {
        let text = format!("{}{DIFF}", header_with_upstream(ok));
        assert!(
            header_problems(&text).is_empty(),
            "{ok}: {:?}",
            header_problems(&text)
        );
    }
    for bad in [
        "maybe",
        "submitted maybe",
        "(none)",
        "https://a b",
        "merged https://x y",
    ] {
        let text = format!("{}{DIFF}", header_with_upstream(bad));
        assert!(
            header_problems(&text)
                .iter()
                .any(|p| p.contains("Upstream:")),
            "{bad} should be refused"
        );
    }
}

#[test]
fn a_header_without_its_upstream_or_its_reason_is_refused() {
    let no_upstream: String = HEADER
        .lines()
        .filter(|l| !l.starts_with("Upstream:"))
        .map(|l| format!("{l}\n"))
        .collect();
    let problems = header_problems(&format!("{no_upstream}{DIFF}"));
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].contains("Upstream:"));

    let empty_upstream = header_with_upstream("");
    assert!(!header_problems(&format!("{empty_upstream}{DIFF}")).is_empty());

    let no_reason = "Subject: Fix it\nUpstream: not yet reported\n---\n";
    let problems = header_problems(&format!("{no_reason}{DIFF}"));
    assert!(problems.iter().any(|p| p.contains("Description:")));
}

#[test]
fn a_bare_description_or_a_bare_separator_is_no_description() {
    // An empty Description: followed by the separator says nothing.
    let empty = "Subject: Fix it\nDescription:\nUpstream: not yet reported\n---\n";
    assert!(
        header_problems(&format!("{empty}{DIFF}"))
            .iter()
            .any(|p| p.contains("Description:"))
    );
    // A bare separator is not description text.
    let bare = "Subject: Fix it\nUpstream: not yet reported\n---\n\n";
    assert!(
        header_problems(&format!("{bare}{DIFF}"))
            .iter()
            .any(|p| p.contains("Description:"))
    );
}

#[test]
fn an_x11_header_and_a_format_patch_mail_pass() {
    // X11 header: Patch-Mainline stands for Upstream:, the text is the description.
    let x11 = "Subject: Fix the cursor\nAuthor: Jane <j@x.org>\nPatch-Mainline: Never\n\
               Git-commit: abc1234\n\nThe cursor was drawn twice on resize.\n---\n";
    assert!(
        header_problems(&format!("{x11}{DIFF}")).is_empty(),
        "{:?}",
        header_problems(x11)
    );
    // git format-patch: From/Subject, then the commit message, then the diffstat.
    let mail = "From 0123456789ab Mon Sep 17 00:00:00 2001\nFrom: Jane <j@x.org>\n\
                Subject: [PATCH] Fix the cursor\nUpstream: not yet reported\n\n\
                The cursor was drawn twice on resize.\n---\n foo.c | 2 +-\n";
    assert!(
        header_problems(&format!("{mail}{DIFF}")).is_empty(),
        "{:?}",
        header_problems(mail)
    );
    // The same mail with an empty message has no description.
    let empty_mail = "From 0123456789ab Mon Sep 17 00:00:00 2001\nFrom: Jane <j@x.org>\n\
                Subject: [PATCH] Fix\nUpstream: not yet reported\n\n---\n";
    assert!(
        header_problems(&format!("{empty_mail}{DIFF}"))
            .iter()
            .any(|p| p.contains("Description:"))
    );
}

#[test]
fn an_added_patch_without_a_header_or_comment_is_refused_by_the_edit_check() {
    let file = "foo-fix.patch";
    let dir = checkout(&spec_with_patch(file), file, DIFF);
    let problems = patch_rule_problems(
        &dir.path().join("foo.spec"),
        BEFORE,
        &spec_with_patch(file),
        None,
    );
    assert!(
        problems.iter().any(|p| p.contains("Subject:")),
        "{problems:?}"
    );
    assert!(problems.iter().all(|p| !p.contains("comment above")));

    let bare_spec = format!("{BEFORE}Patch0: {file}\n");
    let dir = checkout(&bare_spec, file, &format!("{HEADER}{DIFF}"));
    let problems = patch_rule_problems(&dir.path().join("foo.spec"), BEFORE, &bare_spec, None);
    assert!(
        problems.iter().any(|p| p.contains("comment above")),
        "{problems:?}"
    );
}

#[test]
fn a_well_formed_added_patch_passes_the_edit_check() {
    let file = "foo-fix.patch";
    let after = spec_with_patch(file);
    let dir = checkout(&after, file, &format!("{HEADER}{DIFF}"));
    assert!(patch_rule_problems(&dir.path().join("foo.spec"), BEFORE, &after, None).is_empty());
}

#[test]
fn a_patch_the_edit_does_not_add_is_not_judged() {
    let file = "old.patch";
    let before = spec_with_patch(file);
    let dir = checkout(&before, file, DIFF);
    assert!(patch_rule_problems(&dir.path().join("foo.spec"), &before, &before, None).is_empty());
}

#[test]
fn a_renumbered_or_renamed_patch_with_the_same_bytes_is_not_new() {
    let file = "old.patch";
    let before = spec_with_patch(file);
    let dir = checkout(&before, file, DIFF);
    let spec = dir.path().join("foo.spec");
    // The turn renumbers the same patch (Patch0 to Patch3, no comment change).
    let renumbered =
        format!("{BEFORE}# PATCH-FIX-UPSTREAM {file} -- gcc 15 build\nPatch3: {file}\n");
    let snap = CheckoutSnapshot::take(dir.path());
    assert!(added_patches(&spec, &before, &renumbered, Some(&snap)).is_empty());
    assert!(patch_rule_problems(&spec, &before, &renumbered, Some(&snap)).is_empty());
    // A patch that was commented out before, and is switched on with the same bytes.
    let commented = format!("{BEFORE}# PATCH-FIX-UPSTREAM {file} -- old\n#Patch0: {file}\n");
    std::fs::write(&spec, &commented).unwrap();
    let snap = CheckoutSnapshot::take(dir.path());
    let switched_on = format!("{BEFORE}# PATCH-FIX-UPSTREAM {file} -- old\nPatch0: {file}\n");
    assert!(added_patches(&spec, &commented, &switched_on, Some(&snap)).is_empty());
    assert!(patch_rule_problems(&spec, &commented, &switched_on, Some(&snap)).is_empty());
}

#[test]
fn a_changed_patch_is_judged_on_what_its_change_adds() {
    let file = "old.patch";
    // The old patch has no header and no comment: this edit did not add it.
    let before = format!("{BEFORE}Patch0: {file}\n");
    let dir = checkout(&before, file, DIFF);
    let snap = CheckoutSnapshot::take(dir.path());
    let spec = dir.path().join("foo.spec");
    // A rebase edit that only changes source lines passes.
    let rebased =
        "--- foo-1.0/src/foo.c\n+++ foo-1.0/src/foo.c\n@@ -1 +1 @@\n-int x;\n+int x = 1;\n";
    std::fs::write(dir.path().join(file), rebased).unwrap();
    assert!(patch_rule_problems(&spec, &before, &before, Some(&snap)).is_empty());
    // A rebase edit that adds a removal of a test is refused on the change alone.
    let removing = "--- foo-1.0/src/foo.rs\n+++ foo-1.0/src/foo.rs\n@@ -1,2 +1 @@\n-int x;\n-#[test]\n+int x = 0;\n";
    std::fs::write(dir.path().join(file), removing).unwrap();
    let problems = patch_rule_problems(&spec, &before, &before, Some(&snap));
    assert!(
        problems
            .iter()
            .any(|p| p.contains("(changed)") && p.contains("removes a test")),
        "{problems:?}"
    );
    // The missing header of the old patch is not asked for.
    assert!(
        !problems.iter().any(|p| p.contains("Subject:")),
        "{problems:?}"
    );
    // A changed patch that adds `|| true` is refused.
    let hiding = "--- foo-1.0/scripts/run.sh\n+++ foo-1.0/scripts/run.sh\n@@ -1 +1,2 @@\n-int x;\n+int x = 0;\n+run || true\n";
    std::fs::write(dir.path().join(file), hiding).unwrap();
    let problems = patch_rule_problems(&spec, &before, &before, Some(&snap));
    assert!(
        problems.iter().any(|p| p.contains("|| true")),
        "{problems:?}"
    );
}

#[test]
fn a_url_patch_is_judged_on_its_comment_only() {
    let url = "Patch0: https://example.org/fixes/foo-fix.patch";
    let with_comment = format!("{BEFORE}# PATCH-FIX-UPSTREAM foo-fix.patch -- gcc 15\n{url}\n");
    let dir = tempfile::tempdir().unwrap();
    let spec = dir.path().join("foo.spec");
    std::fs::write(&spec, &with_comment).unwrap();
    // No header is read: the file is not in the checkout.
    assert!(patch_rule_problems(&spec, BEFORE, &with_comment, None).is_empty());
    let bare = format!("{BEFORE}{url}\n");
    let problems = patch_rule_problems(&spec, BEFORE, &bare, None);
    assert!(
        problems.iter().any(|p| p.contains("comment above")),
        "{problems:?}"
    );
}

#[test]
fn a_version_in_the_patch_name_is_refused() {
    let raw = "foo-%{version}-fix.patch";
    let after = format!("{BEFORE}# PATCH-FIX-UPSTREAM x -- why\nPatch0: {raw}\n");
    let dir = checkout(&after, "foo-1.0-fix.patch", &format!("{HEADER}{DIFF}"));
    let problems = patch_rule_problems(&dir.path().join("foo.spec"), BEFORE, &after, None);
    assert!(
        problems.iter().any(|p| p.contains("%{version}")),
        "{problems:?}"
    );
}

#[test]
fn a_patch_source_or_a_shell_patch_in_prep_is_refused() {
    let prep = "Name: foo\n%prep\n%autosetup -p1\n";
    let shell = format!("{prep}patch -p1 < %{{SOURCE3}}\n");
    assert!(
        spec_route_problems(prep, &shell)
            .iter()
            .any(|p| p.contains("`patch` command"))
    );
    // A sed -i is refused in an edit that adds a patch, and not in one that adds none.
    let sed = format!("Patch0: fix.patch\n{prep}sed -i 's/a/b/' foo.c\n");
    assert!(
        spec_route_problems(prep, &sed)
            .iter()
            .any(|p| p.contains("sed -i"))
    );
    assert!(spec_route_problems(prep, &format!("{prep}sed -i 's/a/b/' foo.c\n")).is_empty());
    let apply = format!("{prep}git apply foo.patch\n");
    assert!(
        spec_route_problems(prep, &apply)
            .iter()
            .any(|p| p.contains("git apply"))
    );
    let source = format!("{prep}Source3: fix.patch\n");
    assert!(
        spec_route_problems(prep, &source)
            .iter()
            .any(|p| p.contains("Source:"))
    );
    // A sed or patch outside %prep, a macro, and a comment are not refused.
    let build = format!("{prep}%build\nsed -i 's/a/b/' foo.c\n");
    assert!(spec_route_problems(prep, &build).is_empty());
    let macro_line = format!("{prep}%patch -P 3\n# patch -p1 < x\n");
    assert!(spec_route_problems(prep, &macro_line).is_empty());
    // In an edit that adds a patch, a sed that fixes a path or a shebang, or edits the
    // spec, stays; a section after %prep (%conf) is not %prep.
    let fixes = [
        "sed -i 's|/usr/bin/python|/usr/bin/python3|' foo.py",
        "sed -i '1s|^#!.*|#!/usr/bin/python3|' foo.py",
        "sed -i 's/%{version}/1.2/' foo.spec",
    ];
    for line in fixes {
        let edit = format!("Patch0: fix.patch\n{prep}{line}\n");
        assert!(spec_route_problems(prep, &edit).is_empty(), "{line}");
    }
    let conf = format!("Patch0: fix.patch\n{prep}%conf\nsed -i 's/a/b/' foo.c\n");
    assert!(spec_route_problems(prep, &conf).is_empty());
    let generate =
        format!("Patch0: fix.patch\n{prep}%generate_buildrequires\nsed -i 's/a/b/' foo.c\n");
    assert!(spec_route_problems(prep, &generate).is_empty());
}

#[test]
fn a_patch_macro_is_expanded_with_the_spec_defines() {
    let spec = "Name: foo\n%global pkgver 1.2\n%define tag v%{pkgver}\nPatch0: foo-%{pkgver}-fix.patch\nPatch1: %{tag}-x.patch\nPatch2: %{unknown}.patch\n";
    assert_eq!(
        expand_patch_macros("foo-%{pkgver}-fix.patch", spec),
        "foo-1.2-fix.patch"
    );
    assert_eq!(expand_patch_macros("%{tag}-x.patch", spec), "v1.2-x.patch");
    // A macro with no value in the spec stays, so the name is judged unresolved.
    assert!(expand_patch_macros("%{unknown}.patch", spec).contains('%'));
}

#[test]
fn the_changelog_bullet_must_name_each_new_patch_with_a_verb() {
    let file = "foo-fix.patch";
    let after = spec_with_patch(file);
    let dir = checkout(&after, file, &format!("{HEADER}{DIFF}"));
    let spec: &Path = &dir.path().join("foo.spec");
    let names = |log: &str| unnamed_new_patches(spec, BEFORE, log, None);
    assert_eq!(
        names("Add foo-fix.patch: gcc 15 build"),
        Vec::<String>::new()
    );
    assert_eq!(
        names("Backport foo-fix.patch: from upstream"),
        Vec::<String>::new()
    );
    assert_eq!(names("Add patch foo-fix.patch"), Vec::<String>::new());
    assert_eq!(names("Add `foo-fix.patch`: why"), Vec::<String>::new());
    assert_eq!(
        names("Fix the gcc 15 build"),
        vec!["foo-fix.patch".to_string()]
    );
    // A name without its verb, or a verb that is not one of the guideline's, is not named.
    assert_eq!(
        names("Readd foo-fix.patch"),
        vec!["foo-fix.patch".to_string()]
    );
}

#[test]
fn a_url_patch_is_named_by_the_file_name_of_its_url() {
    let url = "Patch0: https://example.org/fixes/foo-fix.patch";
    let after = format!("{BEFORE}# PATCH-FIX-UPSTREAM foo-fix.patch -- gcc 15\n{url}\n");
    let dir = tempfile::tempdir().unwrap();
    let spec = dir.path().join("foo.spec");
    std::fs::write(&spec, &after).unwrap();
    assert_eq!(
        unnamed_new_patches(&spec, BEFORE, "Add foo-fix.patch: why", None),
        Vec::<String>::new()
    );
    assert_eq!(
        unnamed_new_patches(&spec, BEFORE, "Fix the build", None),
        vec!["foo-fix.patch".to_string()]
    );
}

#[test]
fn an_unreferenced_patch_with_the_same_bytes_does_not_make_a_new_line_old() {
    // other.patch is in the checkout but the pre-turn spec does not declare it, so its
    // bytes do not make the turn's new.patch an old patch.
    let dir = tempfile::tempdir().unwrap();
    let spec = dir.path().join("foo.spec");
    std::fs::write(&spec, BEFORE).unwrap();
    std::fs::write(dir.path().join("other.patch"), format!("{HEADER}{DIFF}")).unwrap();
    let snap = CheckoutSnapshot::take(dir.path());
    std::fs::write(dir.path().join("new.patch"), format!("{HEADER}{DIFF}")).unwrap();
    let after = spec_with_patch("new.patch");
    assert_eq!(added_patches(&spec, BEFORE, &after, Some(&snap)).len(), 1);
}

#[test]
fn a_changed_patch_keeps_the_header_its_old_bytes_had() {
    let file = "foo-fix.patch";
    let before = spec_with_patch(file);
    let dir = checkout(&before, file, &format!("{HEADER}{DIFF}"));
    let spec = dir.path().join("foo.spec");
    let snap = CheckoutSnapshot::take(dir.path());
    // The turn rewrites the patch with its Subject: line gone.
    let no_subject = HEADER.replace("Subject: Fix the build with gcc 15\n", "");
    std::fs::write(dir.path().join(file), format!("{no_subject}{DIFF}")).unwrap();
    let problems = patch_rule_problems(&spec, &before, &before, Some(&snap));
    assert!(
        problems.iter().any(|p| p.contains("Subject")),
        "{problems:?}"
    );
}
