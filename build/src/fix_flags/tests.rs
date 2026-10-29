use super::*;
#[test]
fn flags_accumulate_until_the_package_is_submitted() {
    let mut st = PkgState::default();
    settle(&mut st, "r1", vec![FixFlag::CheckRemoved]);
    // A run that makes no commit (AlreadyBuilt, missing_deps) and a later
    // commit that finds nothing new keep what the earlier commit found.
    settle(&mut st, "r2", vec![]);
    assert_eq!(st.fix_flags, vec![FixFlag::CheckRemoved]);
    settle(
        &mut st,
        "r3",
        vec![FixFlag::TestsSkipped {
            items: vec!["a".into()],
        }],
    );
    settle(
        &mut st,
        "r4",
        vec![FixFlag::TestsSkipped {
            items: vec!["a".into(), "b".into()],
        }],
    );
    assert_eq!(
        st.fix_flags,
        vec![
            FixFlag::CheckRemoved,
            FixFlag::TestsSkipped {
                items: vec!["a".into(), "b".into()]
            }
        ]
    );
}

#[test]
fn a_blocked_answer_survives_later_commits() {
    let mut st = PkgState::default();
    settle(
        &mut st,
        "r1",
        vec![FixFlag::WheelSwitch { wheel: "w".into() }],
    );
    add_blocked(&mut st, "r2", "no tests".into());
    settle(&mut st, "r2", vec![FixFlag::CheckRemoved]);
    settle(&mut st, "r3", vec![]);
    assert_eq!(st.fix_flags.len(), 3, "{:?}", st.fix_flags);
    assert!(st.fix_flags.contains(&FixFlag::Blocked {
        why: "no tests".into()
    }));
}

#[test]
fn submitting_a_package_clears_its_flags() {
    let root = tempfile::tempdir().unwrap();
    let ws = Workspace::open(root.path(), "python-foo").unwrap();
    PkgState::update(&ws, |st| settle(st, "r1", vec![FixFlag::CheckRemoved])).unwrap();
    clear_submitted(root.path(), "python-foo").unwrap();
    assert!(of_package(root.path(), "python-foo").is_empty());
}

#[test]
fn submitting_clears_the_workspace_the_gate_reads() {
    // The workspace is named for the RPM name; the OBS spelling is found
    // through the state's `obs_name`, as the gate finds it.
    let root = tempfile::tempdir().unwrap();
    let ws = Workspace::open(root.path(), "python-apscheduler").unwrap();
    PkgState::update(&ws, |st| {
        st.obs_name = Some("python-APScheduler".into());
        settle(st, "r1", vec![FixFlag::CheckRemoved]);
    })
    .unwrap();
    assert_eq!(
        try_of_package(root.path(), "python-APScheduler")
            .unwrap()
            .len(),
        1
    );
    clear_submitted(root.path(), "python-APScheduler").unwrap();
    assert!(
        try_of_package(root.path(), "python-APScheduler")
            .unwrap()
            .is_empty()
    );
}

#[test]
fn commands_follow_elif_and_else_chains() {
    let count = |body: &str| {
        let spec = format!("%check\n{body}%files\n");
        let lines: Vec<&str> = spec.lines().collect();
        commands(&lines, "%check")
    };
    // The `%elif 1` branch runs; the `%if 0` one does not.
    assert_eq!(count("%if 0\n%pytest\n%elif 1\npytest\n%endif\n"), 1);
    // The `%if 1` branch runs, so the `%elif` and `%else` ones do not.
    assert_eq!(count("%if 1\npytest\n%elif 0\nx\n%else\ny\n%endif\n"), 1);
    // An unknown `%elif` may run, and so may the `%else`.
    assert_eq!(count("%if 0\na\n%elif foo\nb\n%else\nc\n%endif\n"), 2);
}

const BASE: &str = "Name: python-foo\nVersion: 1.0\nSource0: foo-1.0.tar.gz\n\
        %prep\n%setup -q\n%build\n%py3_build\n\
        %check\n%pytest\n\
        %install\n%py3_install\n%files\n%{python3_sitelib}/foo\n";

fn with(replace: &str, with: &str) -> String {
    BASE.replace(replace, with)
}

#[test]
fn an_unchanged_spec_has_no_flags() {
    assert!(from_spec_diff(Some(BASE), BASE).is_empty());
}

#[test]
fn a_deselected_test_in_check_is_flagged_by_its_id() {
    let fin = with(
        "%check\n%pytest\n",
        "%check\n# evidence: FAILED tests/test_a.py::test_x\n\
             %pytest --deselect tests/test_a.py::test_x\n",
    );
    assert_eq!(
        from_spec_diff(Some(BASE), &fin),
        vec![FixFlag::TestsSkipped {
            items: vec!["--deselect tests/test_a.py::test_x".into()]
        }]
    );
}

#[test]
fn a_comment_quoting_a_log_is_not_a_skipped_test() {
    let fin = with(
        "%check\n%pytest\n",
        "%check\n# evidence: FAILED t.py -k not --deselect t.py::x\n%pytest\n",
    );
    assert!(from_spec_diff(Some(BASE), &fin).is_empty());
}

#[test]
fn a_keyword_expression_with_not_is_flagged_quoted() {
    let fin = with(
        "%check\n%pytest\n",
        "%check\nPYTEST_ADDOPTS=\"-k 'not test_slow and not test_gpu'\" %pytest\n",
    );
    assert_eq!(
        from_spec_diff(Some(BASE), &fin),
        vec![FixFlag::TestsSkipped {
            items: vec!["-k \"not test_slow and not test_gpu\"".into()]
        }]
    );
}

#[test]
fn an_ignored_file_is_named() {
    let fin = with(
        "%pytest\n%install",
        "%pytest --ignore=tests/test_net.py\n%install",
    );
    assert_eq!(
        from_spec_diff(Some(BASE), &fin),
        vec![FixFlag::TestsSkipped {
            items: vec!["--ignore tests/test_net.py".into()]
        }]
    );
}

#[test]
fn a_test_skip_is_flagged_without_a_baseline_too() {
    let fin = "Name: python-foo\n%check\n%pytest --deselect t.py::a\n";
    assert_eq!(
        from_spec_diff(None, fin),
        vec![FixFlag::TestsSkipped {
            items: vec!["--deselect t.py::a".into()]
        }]
    );
}

#[test]
fn a_removed_check_is_flagged() {
    let fin = BASE.replace("%check\n%pytest\n", "");
    assert_eq!(
        from_spec_diff(Some(BASE), &fin),
        vec![FixFlag::CheckRemoved]
    );
}

#[test]
fn an_emptied_check_is_flagged() {
    let fin = BASE.replace("%check\n%pytest\n", "%check\n# nothing to run\n");
    assert_eq!(
        from_spec_diff(Some(BASE), &fin),
        vec![FixFlag::CheckRemoved]
    );
}

#[test]
fn a_test_command_with_its_failure_ignored_is_flagged_and_is_no_command() {
    let fin = with("%check\n%pytest\n", "%check\n%pytest || true\n");
    assert_eq!(
        from_spec_diff(Some(BASE), &fin),
        vec![
            FixFlag::CheckRemoved,
            FixFlag::TestsSkipped {
                items: vec!["%pytest || true".into()]
            }
        ]
    );
    let fin = with("%check\n%pytest\n", "%check\n%pytest ||:;\n");
    assert!(from_spec_diff(Some(BASE), &fin).contains(&FixFlag::CheckRemoved));
}

#[test]
fn a_check_behind_a_false_conditional_is_removed() {
    let fin = with("%check\n%pytest\n", "%check\n%if 0\n%pytest\n%endif\n");
    assert_eq!(
        from_spec_diff(Some(BASE), &fin),
        vec![
            FixFlag::CheckRemoved,
            FixFlag::TestsSkipped {
                items: vec!["%if 0".into()]
            }
        ]
    );
}

#[test]
fn a_check_behind_a_with_switch_is_flagged_by_its_condition() {
    let fin = with(
        "%check\n%pytest\n",
        "%check\n%if %{with tests}\n%pytest\n%endif\n",
    );
    assert_eq!(
        from_spec_diff(Some(BASE), &fin),
        vec![FixFlag::TestsSkipped {
            items: vec!["%if %{with tests}".into()]
        }]
    );
}

#[test]
fn a_new_syntax_bcond_switched_off_is_flagged() {
    let base = format!("%bcond_without tests\n{BASE}");
    let fin = format!("%bcond tests 0\n{BASE}");
    assert_eq!(
        from_spec_diff(Some(&base), &fin),
        vec![FixFlag::TestsSkipped {
            items: vec!["%bcond tests 0".into()]
        }]
    );
}

#[test]
fn a_global_that_switches_the_tests_off_is_flagged() {
    let fin = format!("%global _without_tests 1\n{BASE}");
    assert_eq!(
        from_spec_diff(Some(BASE), &fin),
        vec![FixFlag::TestsSkipped {
            items: vec!["%global _without_tests 1".into()]
        }]
    );
}

#[test]
fn a_keyword_or_marker_selection_is_flagged_positive_or_negative() {
    for (line, item) in [
        ("%pytest -k \"(not slow)\"", "-k \"(not slow)\""),
        ("%pytest -k test_fast", "-k \"test_fast\""),
        ("%pytest -m smoke", "-m \"smoke\""),
        ("%pytest -k\"not slow\"", "-k \"not slow\""),
    ] {
        let fin = with("%check\n%pytest\n", &format!("%check\n{line}\n"));
        assert_eq!(
            from_spec_diff(Some(BASE), &fin),
            vec![FixFlag::TestsSkipped {
                items: vec![item.into()]
            }],
            "{line}"
        );
    }
}

#[test]
fn a_pytest_line_running_one_file_is_flagged_and_its_option_values_are_not() {
    let fin = with("%check\n%pytest\n", "%check\n%pytest tests/test_a.py\n");
    assert_eq!(
        from_spec_diff(Some(BASE), &fin),
        vec![FixFlag::TestsSkipped {
            items: vec!["only tests/test_a.py".into()]
        }]
    );
    // Two files, or a file as an option's value: not a single-file run.
    let two = with(
        "%check\n%pytest\n",
        "%check\n%pytest tests/a.py tests/b.py\n",
    );
    assert!(from_spec_diff(Some(BASE), &two).is_empty());
}

#[test]
fn a_ctest_exclusion_is_flagged_under_either_spelling() {
    let fin = with("%check\n%pytest\n", "%check\nctest --exclude-regex slow_\n");
    assert_eq!(
        from_spec_diff(Some(BASE), &fin),
        vec![FixFlag::TestsSkipped {
            items: vec!["ctest --exclude-regex slow_".into()]
        }]
    );
}

#[test]
fn a_test_directory_removed_in_prep_is_flagged() {
    let fin = with("%setup -q\n", "%setup -q\nrm -rf tests\n");
    assert_eq!(
        from_spec_diff(Some(BASE), &fin),
        vec![FixFlag::TestsSkipped {
            items: vec!["rm -rf tests".into()]
        }]
    );
}

#[test]
fn a_second_wheel_beside_the_baseline_one_is_a_switch() {
    let base = format!("Source1: a-1.0-py3-none-any.whl\n{BASE}");
    let fin = format!("Source1: a-1.0-py3-none-any.whl\nSource2: b-1.0-py3-none-any.whl\n{BASE}");
    assert_eq!(
        from_spec_diff(Some(&base), &fin),
        vec![FixFlag::WheelSwitch {
            wheel: "b-1.0-py3-none-any.whl".into()
        }]
    );
}

#[test]
fn a_check_that_was_already_empty_is_not_flagged() {
    let base = BASE.replace("%check\n%pytest\n", "%check\n");
    assert!(from_spec_diff(Some(&base), &base).is_empty());
}

#[test]
fn a_test_toggle_switched_off_by_bcond_is_flagged() {
    let base = format!("%bcond_without tests\n{BASE}");
    let fin = format!("%bcond_with tests\n{BASE}");
    assert_eq!(
        from_spec_diff(Some(&base), &fin),
        vec![FixFlag::TestsSkipped {
            items: vec!["%bcond_with tests".into()]
        }]
    );
}

#[test]
fn a_macro_guard_in_check_is_flagged_by_its_line() {
    let fin = with(
        "%check\n%pytest\n",
        "%check\n%{!?with_slowtests:%pytest -k 'not slow'}\n",
    );
    let flags = from_spec_diff(Some(BASE), &fin);
    assert!(
        matches!(&flags[0], FixFlag::TestsSkipped { items } if items.iter().any(|i| i.contains("with_slowtests"))),
        "{flags:?}"
    );
}

#[test]
fn a_wheel_put_in_for_a_source_build_is_flagged_by_its_file() {
    let fin = with(
        "Source0: foo-1.0.tar.gz",
        "Source0:        foo-1.0-py3-none-any.whl",
    );
    assert_eq!(
        from_spec_diff(Some(BASE), &fin),
        vec![FixFlag::WheelSwitch {
            wheel: "foo-1.0-py3-none-any.whl".into()
        }]
    );
}

#[test]
fn a_new_package_built_from_a_wheel_is_not_a_switch() {
    let fin = "Name: python-foo\nSource0:        foo-1.0-py3-none-any.whl\n";
    assert!(from_spec_diff(None, fin).is_empty());
}

#[test]
fn a_line_is_one_line_and_capped() {
    let why = "word ".repeat(200);
    let line = FixFlag::Blocked { why }.line();
    assert!(!line.contains('\n'));
    assert!(line.chars().count() <= LINE_CAP + "FLAG: a fix turn answered blocked: ...".len());
    assert!(line.ends_with("..."));
}

#[test]
fn settle_keeps_blocked_answers_and_the_spec_flags_of_earlier_commits() {
    let mut st = PkgState::default();
    add_blocked(&mut st, "r1", "no gcc".into());
    settle(&mut st, "r1", vec![FixFlag::CheckRemoved]);
    settle(&mut st, "r1", vec![]);
    assert_eq!(
        st.fix_flags,
        vec![
            FixFlag::Blocked {
                why: "no gcc".into()
            },
            FixFlag::CheckRemoved
        ]
    );
    // A later run adds to them, and owns the run id from then on.
    settle(&mut st, "r2", vec![FixFlag::CheckRemoved]);
    assert_eq!(st.fix_flags.len(), 2);
    assert_eq!(st.fix_flags_run_id.as_deref(), Some("r2"));
}

#[test]
fn a_reader_outside_the_run_sees_the_flags_the_package_recorded() {
    let root = tempfile::tempdir().unwrap();
    assert!(of_package(root.path(), "python-foo").is_empty());
    let ws = Workspace::open(root.path(), "python-foo").unwrap();
    PkgState::update(&ws, |st| {
        settle(st, "r1", vec![FixFlag::CheckRemoved]);
    })
    .unwrap();
    assert_eq!(
        of_package(root.path(), "python-foo"),
        vec![FixFlag::CheckRemoved]
    );
}

#[test]
fn the_state_round_trips_the_flags() {
    let mut st = PkgState::default();
    settle(
        &mut st,
        "r1",
        vec![FixFlag::WheelSwitch {
            wheel: "a.whl".into(),
        }],
    );
    let text = serde_json::to_string(&st).unwrap();
    let back: PkgState = serde_json::from_str(&text).unwrap();
    assert_eq!(back.fix_flags, st.fix_flags);
}
