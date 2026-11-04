use super::*;
use std::collections::BTreeSet;

fn spec(prep: &str) -> String {
    format!(
        "Name: foo\nVersion: 1\nRelease: 0\nSummary: x\nLicense: MIT\nSource0: foo-1.tar.gz\n\
         Patch0: a.patch\nPatch1: b.patch\nBuildRequires: gcc\n\n%description\nx\n\n\
         %prep\n{prep}\n%build\nmake\n\n%check\ntrue\n"
    )
}

/// `%setup` and one `%patch -P N` line per patch.
fn s() -> String {
    spec("%setup -q\n%patch -P 0 -p1\n%patch -P 1 -p1\n")
}

/// `%autosetup -p1`.
fn a() -> String {
    spec("%autosetup -p1\n")
}

fn edit(text: &str, from: &str, to: &str) -> String {
    assert!(text.contains(from), "{from:?} is not in the spec");
    text.replacen(from, to, 1)
}

fn disabled(before: &str, after: &str) -> Vec<(String, Disabled)> {
    disabled_patches(before, after)
}

fn not_applied(names: &[&str]) -> Vec<(String, Disabled)> {
    names
        .iter()
        .map(|n| (n.to_string(), Disabled::NotApplied))
        .collect()
}

fn conditional(names: &[&str], guard: &str) -> Vec<(String, Disabled)> {
    names
        .iter()
        .map(|n| {
            (
                n.to_string(),
                Disabled::Conditional {
                    guard: guard.to_string(),
                },
            )
        })
        .collect()
}

#[test]
fn a_deleted_patch_line_disables() {
    let after = edit(&s(), "%patch -P 1 -p1\n", "");
    assert_eq!(disabled(&s(), &after), not_applied(&["b.patch"]));
}

#[test]
fn a_deleted_numbered_patch_line_disables() {
    let before = spec("%setup -q\n%patch0 -p1\n%patch1 -p1\n");
    let after = edit(&before, "%patch1 -p1\n", "");
    assert_eq!(disabled(&before, &after), not_applied(&["b.patch"]));
}

#[test]
fn a_commented_patch_line_disables() {
    for (from, to) in [
        ("%patch -P 1 -p1", "# %patch -P 1 -p1"),
        ("%patch -P 1 -p1", "#%patch -P 1 -p1"),
    ] {
        let after = edit(&s(), from, to);
        assert_eq!(disabled(&s(), &after), not_applied(&["b.patch"]), "{to}");
    }
    let before = spec("%setup -q\n%patch0 -p1\n%patch1 -p1\n");
    let after = edit(&before, "%patch1 -p1", "#%patch1 -p1");
    assert_eq!(disabled(&before, &after), not_applied(&["b.patch"]));
}

#[test]
fn autosetup_with_n_disables_every_patch() {
    let after = edit(&a(), "%autosetup -p1", "%autosetup -p1 -N");
    assert_eq!(disabled(&a(), &after), not_applied(&["a.patch", "b.patch"]));
}

#[test]
fn a_narrower_autopatch_disables_what_it_leaves_out() {
    let before = spec("%autosetup -N\n%autopatch -p1\n");
    let range = edit(&before, "%autopatch -p1", "%autopatch -p1 -M 0");
    assert_eq!(disabled(&before, &range), not_applied(&["b.patch"]));
    let list = edit(&before, "%autopatch -p1", "%autopatch -p1 0");
    assert_eq!(disabled(&before, &list), not_applied(&["b.patch"]));
    let glued = edit(&before, "%autopatch -p1", "%autopatch -p1 -m1");
    assert_eq!(disabled(&before, &glued), not_applied(&["a.patch"]));
}

#[test]
fn a_false_condition_around_the_declaration_is_not_declared() {
    let after = edit(
        &a(),
        "Patch1: b.patch\n",
        "%if 0\nPatch1: b.patch\n%endif\n",
    );
    assert_eq!(
        disabled(&a(), &after),
        [(
            "b.patch".to_string(),
            Disabled::NotDeclared {
                guard: "%if 0 [then]".to_string()
            }
        )]
    );
}

#[test]
fn a_false_condition_around_the_application_disables() {
    let after = edit(
        &s(),
        "%patch -P 1 -p1\n",
        "%if 0\n%patch -P 1 -p1\n%endif\n",
    );
    assert_eq!(disabled(&s(), &after), not_applied(&["b.patch"]));
}

#[test]
fn a_new_guard_the_reader_cannot_evaluate_is_a_condition() {
    let after = edit(
        &s(),
        "%patch -P 1 -p1\n",
        "%if %{without x}\n%patch -P 1 -p1\n%endif\n",
    );
    assert_eq!(
        disabled(&s(), &after),
        conditional(&["b.patch"], "%if %{without x} [then]")
    );
    let after = edit(
        &s(),
        "%patch -P 1 -p1\n",
        "%ifarch x86_64\n%patch -P 1 -p1\n%endif\n",
    );
    assert_eq!(
        disabled(&s(), &after),
        conditional(&["b.patch"], "%ifarch x86_64 [then]")
    );
    let after = edit(
        &a(),
        "Patch1: b.patch\n",
        "%if %{with x}\nPatch1: b.patch\n%endif\n",
    );
    assert_eq!(
        disabled(&a(), &after),
        conditional(&["b.patch"], "%if %{with x} [then]")
    );
}

#[test]
fn an_application_moved_out_of_prep_disables() {
    let after = edit(&s(), "%patch -P 1 -p1\n", "");
    let after = edit(&after, "make\n", "make\n%patch -P 1 -p1\n");
    assert_eq!(disabled(&s(), &after), not_applied(&["b.patch"]));
}

#[test]
fn a_reversed_patch_disables() {
    let after = edit(&s(), "%patch -P 1 -p1", "%patch -P 1 -p1 -R");
    assert_eq!(disabled(&s(), &after), not_applied(&["b.patch"]));
}

#[test]
fn a_shell_applied_patch_is_read_and_its_deletion_disables() {
    let before = spec("%setup -q\n%patch -P 0 -p1\npatch -p1 < %{PATCH1}\n");
    let after = edit(&before, "patch -p1 < %{PATCH1}\n", "");
    assert_eq!(disabled(&before, &after), not_applied(&["b.patch"]));
    let before = spec("%setup -q\n%patch -P 0 -p1\ngit apply %{P:1}\n");
    let after = edit(&before, "git apply %{P:1}\n", "");
    assert_eq!(disabled(&before, &after), not_applied(&["b.patch"]));
}

#[test]
fn a_positional_patch_number_is_read() {
    let before = spec("%setup -q\n%patch 0 -p1\n%patch 1 -p1\n");
    let after = edit(&before, "%patch 1 -p1\n", "");
    assert_eq!(disabled(&before, &after), not_applied(&["b.patch"]));
}

#[test]
fn losing_one_branch_of_a_conditional_application_is_a_condition() {
    let both = "%if %{with x}\n%autosetup -p1\n%else\n%autosetup -p1\n%endif\n";
    let before = spec(both);
    let after = edit(&before, "%else\n%autosetup -p1", "%else\n%autosetup -p1 -N");
    assert_eq!(
        disabled(&before, &after),
        conditional(&["a.patch", "b.patch"], "%if %{with x} [else]")
    );
}

#[test]
fn edits_that_keep_the_patches_applied_disable_nothing() {
    let none = Vec::<(String, Disabled)>::new();
    // An unrelated edit.
    let after = edit(
        &s(),
        "BuildRequires: gcc\n",
        "BuildRequires: gcc\nBuildRequires: make\n",
    );
    assert_eq!(disabled(&s(), &after), none);
    let after = edit(&s(), "true\n", "make test\n");
    assert_eq!(disabled(&s(), &after), none);
    // The same application in another form.
    let before = spec("%setup -q\n%patch0 -p1\n%patch1 -p1\n");
    for form in [
        "%patch -P 0 -p1\n%patch -P 1 -p1\n",
        "%patch 0 -p1\n%patch 1 -p1\n",
    ] {
        let after = spec(&format!("%setup -q\n{form}"));
        assert_eq!(disabled(&before, &after), none, "{form}");
    }
    // Explicit lines replaced by %autosetup, and the lines reordered.
    assert_eq!(disabled(&s(), &a()), none);
    let swapped = spec("%setup -q\n%patch -P 1 -p1\n%patch -P 0 -p1\n");
    assert_eq!(disabled(&s(), &swapped), none);
}

#[test]
fn an_unknown_guard_left_as_it_was_is_not_a_change() {
    let guarded = |indent: &str| {
        spec(&format!(
            "%setup -q\n%patch -P 0 -p1\n%if 0%{{?suse_version}} > 1500\n{indent}%patch -P 1 -p1\n%endif\n"
        ))
    };
    assert!(disabled(&guarded(""), &guarded("  ")).is_empty());
    let reflowed = edit(
        &guarded(""),
        "%if 0%{?suse_version} > 1500",
        "%if  0%{?suse_version}   > 1500",
    );
    assert!(disabled(&guarded(""), &reflowed).is_empty());
}

#[test]
fn a_guard_that_goes_away_is_not_a_disable() {
    let before = spec("%setup -q\n%patch -P 0 -p1\n%ifarch x86_64\n%patch -P 1 -p1\n%endif\n");
    assert!(disabled(&before, &s()).is_empty());
}

#[test]
fn a_taken_condition_around_the_application_is_not_a_guard() {
    let after = edit(
        &s(),
        "%patch -P 1 -p1\n",
        "%if 1\n%patch -P 1 -p1\n%endif\n",
    );
    assert!(disabled(&s(), &after).is_empty());
}

#[test]
fn an_unreadable_autopatch_range_is_a_change_only_when_the_line_changes() {
    let before = spec("%autosetup -N\n%autopatch -p1 -m %{lo}\n");
    assert!(disabled(&before, &before).is_empty());
    let after = edit(&before, "-m %{lo}", "-m %{hi}");
    assert_eq!(
        disabled(&before, &after),
        conditional(&["a.patch", "b.patch"], "%autopatch -p1 -m %{hi}")
    );
}

#[test]
fn an_endif_with_a_comment_does_not_hide_the_following_lines() {
    let text = spec("%setup -q\n%patch -P 0 -p1\n%if 0%{?x}\n%endif # x\n%patch -P 1 -p1\n");
    assert!(disabled(&text, &text).is_empty());
    assert_eq!(
        applied_paths(&text)["b.patch"],
        BTreeSet::from([Vec::<String>::new()])
    );
}

#[test]
fn the_pipelines_own_edits_disable_nothing() {
    let before = a();
    let bumped = crate::spec::set_version(&before, "2.0").expect("the version is set");
    assert_ne!(bumped, before);
    assert!(disabled(&before, &bumped).is_empty());
    let named = spec("%autosetup -p1 -n foo-1\n");
    let fixed = crate::spec::fix_autosetup_dir(&named, "foo-2.0").expect("the directory is fixed");
    assert_ne!(fixed, named);
    assert!(disabled(&named, &fixed).is_empty());
    let dotted = edit(&named, "Version: 1\n", "Version: 1.2.3\n");
    let dotted = edit(&dotted, "-n foo-1", "-n foo-1.2.3");
    let tidy = crate::spec_tidy::use_version_macro(&dotted);
    assert_ne!(tidy, dotted);
    assert!(disabled(&dotted, &tidy).is_empty());
}

#[test]
fn a_patch_with_no_application_in_the_before_spec_is_never_disabled() {
    let before = spec("%setup -q\n%my_apply_patches\n");
    let after = spec("%setup -q\n");
    assert!(disabled(&before, &after).is_empty());
}

#[test]
fn a_removed_declaration_is_a_removal_not_a_disable() {
    let after = edit(&s(), "Patch1: b.patch\n", "");
    assert!(disabled(&s(), &after).is_empty());
}

fn with_macros(define: &str, value: &str) -> String {
    spec(&format!(
        "%setup -q\n%patch -P 0 -p1\n%if %{{use_patches}}\n%patch -P 1 -p1\n%endif\n"
    ))
    .replacen(
        "Name: foo\n",
        &format!("%global {define} {value}\nName: foo\n"),
        1,
    )
}

#[test]
fn a_changed_macro_behind_an_unknown_guard_is_a_condition() {
    let before = with_macros("use_patches", "1");
    let after = with_macros("use_patches", "0");
    let got = disabled(&before, &after);
    assert_eq!(got.len(), 1, "{got:?}");
    assert!(
        matches!(&got[0].1, Disabled::Conditional { guard } if guard.contains("use_patches=0"))
    );
    // The same value, and a define the guard does not name, change nothing.
    assert!(disabled(&before, &before).is_empty());
    let unrelated = edit(&before, "Name: foo\n", "%global other 2\nName: foo\n");
    assert!(disabled(&before, &unrelated).is_empty());
}

#[test]
fn a_changed_define_in_a_comparison_guard_is_a_condition() {
    let guarded = |ver: &str| {
        spec("%setup -q\n%patch -P 0 -p1\n%if %{pyver} > 310\n%patch -P 1 -p1\n%endif\n").replacen(
            "Name: foo\n",
            &format!("%define pyver {ver}\nName: foo\n"),
            1,
        )
    };
    assert!(!disabled(&guarded("311"), &guarded("310")).is_empty());
    assert!(disabled(&guarded("311"), &guarded("311")).is_empty());
}

fn guarded(bcond: &str, condition: &str) -> String {
    spec(&format!(
        "{bcond}\n%setup -q\n%patch -P 0 -p1\n{condition}\n%patch -P 1 -p1\n%endif\n"
    ))
}

#[test]
fn a_new_guard_that_is_on_by_default_keeps_the_patch_applied() {
    for (bcond, condition) in [
        ("%bcond_without x", "%if %{with x}"),
        ("%bcond x 1", "%if %{with x}"),
        ("%bcond_with x", "%if %{without x}"),
        ("%bcond_with x", "%if !%{with x}"),
    ] {
        let after = guarded(bcond, condition);
        assert!(disabled(&s(), &after).is_empty(), "{bcond}: {condition}");
    }
}

#[test]
fn a_new_guard_that_is_off_by_default_disables() {
    for (bcond, condition) in [
        ("%bcond_with x", "%if %{with x}"),
        ("%bcond_without x", "%if %{without x}"),
        ("%bcond x 0", "%if %{with x}"),
    ] {
        let after = guarded(bcond, condition);
        assert_eq!(
            disabled(&s(), &after),
            not_applied(&["b.patch"]),
            "{bcond}: {condition}"
        );
    }
}

#[test]
fn a_guard_on_a_bcond_the_spec_does_not_declare_is_a_condition() {
    let after = guarded("", "%if %{with x}");
    assert_eq!(
        disabled(&s(), &after),
        conditional(&["b.patch"], "%if %{with x} [then]")
    );
}

#[test]
fn only_a_new_narrowing_line_narrows() {
    assert!(narrows_applications(
        &a(),
        &edit(&a(), "%autosetup -p1", "%autosetup -p1 -N")
    ));
    let auto = spec("%autosetup -N\n%autopatch -p1\n");
    assert!(narrows_applications(
        &auto,
        &edit(&auto, "%autopatch -p1", "%autopatch -p1 -M 0")
    ));
    assert!(narrows_applications(
        &auto,
        &edit(&auto, "%autopatch -p1", "%autopatch -p1 1")
    ));
    // Unchanged narrowing, a comment, and widening do not.
    assert!(!narrows_applications(&auto, &auto));
    assert!(!narrows_applications(
        &auto,
        &edit(&auto, "%autopatch -p1", "%autopatch  -p1")
    ));
    assert!(!narrows_applications(
        &edit(&a(), "%autosetup -p1", "%autosetup -p1 -N"),
        &a()
    ));
    assert!(!narrows_applications(
        &a(),
        &edit(&a(), "%autosetup -p1", "%autosetup -p1\n# %autosetup -N")
    ));
    assert!(!narrows_applications(
        &s(),
        &edit(&s(), "%patch -P 1 -p1\n", "")
    ));
}
