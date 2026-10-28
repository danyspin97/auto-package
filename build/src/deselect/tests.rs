use super::*;

const SPEC: &str = "Name: python-x\n%prep\n%autosetup -p1\n\n%build\n%pyproject_wheel\n\n%check\n%pytest\n\n%files\n%license LICENSE\n";

fn none(_: &str) -> bool {
    false
}

fn pytest_log(failed: &[&str], passed: usize) -> String {
    let mut s = String::from("[  30s] =========== short test summary info ============\n");
    for f in failed {
        s.push_str(&format!("[  30s] FAILED {f} - AssertionError: boom\n"));
    }
    s.push_str(&format!(
        "[  30s] ===== {} failed, {passed} passed in 1.2s =====\n",
        failed.len()
    ));
    s
}

#[test]
fn pytest_failures_become_a_narrow_deselect_with_evidence() {
    let log = pytest_log(
        &[
            "tests/test_a.py::TestX::test_one",
            "tests/test_b.py::test_two[a-b]",
        ],
        40,
    );
    let got = drop_failing_tests(SPEC, &log, &none).unwrap();
    assert_eq!(got.tests.len(), 2);
    assert_eq!(got.omitted, 0);
    assert!(
        got.spec
            .contains("# evidence: FAILED tests/test_a.py::TestX::test_one - AssertionError: boom"),
        "{}",
        got.spec
    );
    assert!(got.spec.contains(
            "export PYTEST_ADDOPTS=\"${PYTEST_ADDOPTS:-} --deselect=tests/test_a.py::TestX::test_one --deselect=tests/test_b.py::test_two[a-b]\""
        ), "{}", got.spec);
    // Inside %check, before the %pytest run.
    let check = got.spec.split("%check\n").nth(1).unwrap();
    assert!(check.find("export PYTEST_ADDOPTS").unwrap() < check.find("%pytest").unwrap());
    // Dropping the same tests again is refused: the drop is not taking effect.
    let again = drop_failing_tests(&got.spec, &log, &none).unwrap_err();
    assert!(again.contains("already dropped"), "{again}");
}

#[test]
fn a_pytest_id_that_prefixes_another_failing_id_is_not_dropped() {
    // `--deselect=tests/io.py::test_read` would also deselect `test_read_large`.
    let log = pytest_log(
        &["tests/io.py::test_read", "tests/io.py::test_read_large"],
        40,
    );
    let got = drop_failing_tests(SPEC, &log, &none).unwrap();
    assert_eq!(
        got.tests.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(),
        ["tests/io.py::test_read_large"]
    );
    assert_eq!(got.omitted, 1);
    let spec = &got.spec;
    assert!(
        spec.contains("--deselect=tests/io.py::test_read_large"),
        "{spec}"
    );
    assert!(
        !spec.contains("--deselect=tests/io.py::test_read "),
        "{spec}"
    );
    assert!(
        !spec.contains("--deselect=tests/io.py::test_read\""),
        "{spec}"
    );
    assert!(
        spec.contains("--deselect matches node ids by prefix"),
        "{spec}"
    );

    // Once the longer id is dropped, the shorter one still cannot be dropped exactly.
    let longer = drop_failing_tests(SPEC, &log, &none).unwrap();
    let refused = drop_failing_tests(&longer.spec, &log, &none).unwrap_err();
    assert!(refused.contains("cannot be dropped exactly"), "{refused}");

    // A lone failing id with no extension in the log is dropped as before.
    let alone =
        drop_failing_tests(SPEC, &pytest_log(&["tests/io.py::test_read"], 40), &none).unwrap();
    assert_eq!(alone.tests.len(), 1);
    assert_eq!(alone.omitted, 0);
}

#[test]
fn at_most_five_tests_per_round_and_fifteen_per_spec() {
    let ids: Vec<String> = (0..9).map(|i| format!("tests/t.py::test_{i}")).collect();
    let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
    let got = drop_failing_tests(SPEC, &pytest_log(&refs, 100), &none).unwrap();
    assert_eq!(got.tests.len(), MAX_PER_ROUND);
    assert_eq!(got.omitted, 4);
    assert_eq!(dropped_count(&got.spec), 5);
    // A spec already at the cap takes no more.
    let mut spec = SPEC.to_string();
    for i in 0..MAX_IN_SPEC {
        spec.push_str(&format!("# evidence: FAILED t{i}\n"));
    }
    let err = drop_failing_tests(&spec, &pytest_log(&refs, 100), &none).unwrap_err();
    assert!(err.contains("already dropped"), "{err}");
}

#[test]
fn a_whole_file_or_directory_or_an_unsafe_id_is_never_dropped() {
    for bad in [
        "tests/test_a.py",
        "tests/",
        "tests/test_a.py::",
        "/abs/test_a.py::t",
        "../x/test_a.py::t",
        "tests/test_a.py::t[a b]",
        "tests/test_a.py::t[$(x)]",
        "tests/test_a.py::t[100%]",
        "tests/test_a.py::test_very_long[param-...",
    ] {
        assert!(!safe_pytest_id(bad), "{bad}");
    }
    let err = drop_failing_tests(SPEC, &pytest_log(&["tests/test_a.py"], 5), &none).unwrap_err();
    assert!(err.contains("no individual failing test"), "{err}");
}

#[test]
fn import_and_collection_errors_are_a_dependency_problem() {
    let collection = "[  9s] ERROR collecting tests/test_a.py\n[  9s] E   ModuleNotFoundError: No module named 'numpy'\n[  9s] ===== 1 error in 0.3s =====\n";
    let err = drop_failing_tests(SPEC, collection, &none).unwrap_err();
    assert!(err.contains("dependency problem"), "{err}");
    let summary = "FAILED tests/test_a.py::test_x - ImportError: cannot import name 'y' from 'z'\n== 1 failed, 9 passed in 1s ==\n";
    assert!(
        drop_failing_tests(SPEC, summary, &none)
            .unwrap_err()
            .contains("dependency problem")
    );
    let in_test = "E   ModuleNotFoundError: No module named 'torch'\nFAILED tests/test_a.py::test_x - assert 0\n== 1 failed, 9 passed in 1s ==\n";
    assert!(drop_failing_tests(SPEC, in_test, &none).is_err());
    let unittest = "ERROR: tests.test_a (unittest.loader._FailedTest.tests.test_a)\n";
    assert!(
        drop_failing_tests(SPEC, unittest, &none)
            .unwrap_err()
            .contains("dependency problem")
    );
    let broken = "ERROR tests/test_a.py - SyntaxError: bad\n== 1 error in 0.1s ==\n";
    assert!(drop_failing_tests(SPEC, broken, &none).is_err());
}

#[test]
fn a_failure_message_with_failed_and_in_is_not_the_summary() {
    // The message of a test that did pass the suite, and says `failed ... in`.
    let log = "[  30s] =========== short test summary info ============\n\
                   [  30s] FAILED tests/t.py::test_a - request failed in retry loop\n\
                   [  30s] ===== 1 failed, 4 passed in 1.2s =====\n";
    let found = failing_tests(log).unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].id, "tests/t.py::test_a");
    // The summary still decides: nothing passed means the suite is not dropped.
    let none_passed = log.replace("4 passed", "0 passed");
    assert!(
        failing_tests(&none_passed)
            .unwrap_err()
            .contains("no test passed")
    );
    assert_eq!(summary_head("request failed in retry loop"), None);
    assert_eq!(
        summary_head("3 failed, 5 passed in 1.2s"),
        Some("3 failed, 5 passed")
    );
}

#[test]
fn a_suite_in_which_nothing_passes_is_not_dropped() {
    let err = drop_failing_tests(SPEC, &pytest_log(&["tests/t.py::test_a"], 0), &none).unwrap_err();
    assert!(err.contains("no test passed"), "{err}");
}

#[test]
fn the_export_goes_after_a_pytest_addopts_the_check_assigns_outright() {
    let spec = "%check\nexport PYTEST_ADDOPTS=\"-x\"\n%pytest\n\n%files\n";
    let got = drop_failing_tests(spec, &pytest_log(&["tests/t.py::test_a"], 5), &none).unwrap();
    let export_x = got.spec.find("PYTEST_ADDOPTS=\"-x\"").unwrap();
    let ours = got.spec.find("--deselect=").unwrap();
    assert!(export_x < ours, "{}", got.spec);
    let multiline = "%check\nexport PYTEST_ADDOPTS=\"-x \\\n  -q\"\n%pytest\n";
    assert!(drop_failing_tests(multiline, &pytest_log(&["tests/t.py::test_a"], 5), &none).is_err());
}

#[test]
fn ctest_failures_narrow_the_ctest_line() {
    let log = "[  50s] 90% tests passed, 2 tests failed out of 20\n[  50s] \n[  50s] The following tests FAILED:\n[  50s] \t  3 - test_foo.bar (Failed)\n[  50s] \t  7 - test_baz (Timeout)\n[  50s] Errors while running CTest\n";
    let spec = "%check\n%ctest\n\n%files\n";
    let got = drop_failing_tests(spec, log, &none).unwrap();
    assert_eq!(got.tests.len(), 2);
    assert!(
        got.spec.contains("%ctest -E '^(test_foo\\.bar|test_baz)$'"),
        "{}",
        got.spec
    );
    assert!(
        got.spec.contains("# evidence: 3 - test_foo.bar (Failed)"),
        "{}",
        got.spec
    );
    // A test ctest could not run is a build problem.
    let not_run = "The following tests FAILED:\n\t  1 - test_a (Not Run)\n";
    assert!(
        drop_failing_tests(spec, not_run, &none)
            .unwrap_err()
            .contains("build problem")
    );
    // An existing exclude is not merged blindly.
    let with_e = "%check\n%ctest -E slow\n";
    assert!(drop_failing_tests(with_e, log, &none).is_err());
}

#[test]
fn unittest_failures_get_a_skip_decorator_in_prep_for_the_module_found() {
    let log = "[  20s] FAIL: test_roundtrip (pkg.tests.test_codec.CodecTest.test_roundtrip)\n[  20s] ERROR: test_other (pkg.tests.test_codec.CodecTest)\n[  20s] FAIL: test_gone (pkg.tests.test_gone.G.test_gone)\n";
    let spec = "%prep\n%autosetup\n\n%build\n%check\n%python_exec -m unittest\n";
    let have = |p: &str| p == "pkg/tests/test_codec.py";
    let got = drop_failing_tests(spec, log, &have).unwrap();
    assert!(got.spec.contains("def test_roundtrip\\("), "{}", got.spec);
    assert!(
        got.spec.contains("/^class CodecTest\\b/,/^class /"),
        "{}",
        got.spec
    );
    assert!(got.spec.contains("pkg/tests/test_codec.py"), "{}", got.spec);
    assert!(!got.spec.contains("test_gone"), "{}", got.spec);
    let prep = got.spec.split("%prep\n").nth(1).unwrap();
    assert!(prep.find("sed -i").unwrap() < prep.find("%build").unwrap());
    // Nothing found in the tree: refused.
    assert!(drop_failing_tests(spec, log, &none).is_err());
}

#[test]
fn a_percent_in_the_evidence_is_escaped_for_rpm() {
    let log =
        "FAILED tests/t.py::test_a - AssertionError: 100% wrong\n== 1 failed, 4 passed in 1s ==\n";
    let got = drop_failing_tests(SPEC, log, &none).unwrap();
    assert!(got.spec.contains("100%% wrong"), "{}", got.spec);
}

const INSTALLED_LOG: &str = "[  20s] FAIL: test_roundtrip (pkg.tests.test_codec.CodecTest.test_roundtrip)\n[  20s] ERROR: test_other (pkg.tests.test_codec.CodecTest)\n";

fn installed_spec(check: &str) -> String {
    format!(
        "Name: python-pkg\nBuildRequires:  %{{python_module pip}}\nBuildRequires:  python-rpm-macros\n\n%prep\n%autosetup\n\n%build\n%pyproject_wheel\n\n%check\n{check}\n\n%files\n"
    )
}

#[test]
fn installed_module_unittest_failures_move_the_check_to_pytest_with_a_k_expression() {
    let spec = installed_spec("%python_exec -m unittest -v pkg.tests");
    let got = drop_failing_tests(&spec, INSTALLED_LOG, &none).unwrap();
    assert_eq!(got.tests.len(), 2);
    assert!(
        got.spec
            .contains("%python_exec -m pytest -v --pyargs pkg.tests"),
        "{}",
        got.spec
    );
    assert!(
        got.spec.contains("BuildRequires:  %{python_module pytest}"),
        "{}",
        got.spec
    );
    assert!(
        got.spec.contains(
            "# evidence: FAIL: test_roundtrip (pkg.tests.test_codec.CodecTest.test_roundtrip)"
        ),
        "{}",
        got.spec
    );
    assert!(got.spec.contains("-k 'not (test_codec and CodecTest and test_roundtrip) and not (test_codec and CodecTest and test_other)'"), "{}", got.spec);
    let check = got.spec.split("%check\n").nth(1).unwrap();
    assert!(check.find("-k 'not").unwrap() < check.find("-m pytest").unwrap());
    // The BuildRequires sits with the others, before %prep.
    assert!(got.spec.find("python_module pytest").unwrap() < got.spec.find("%prep").unwrap());
    assert_eq!(dropped_count(&got.spec), 2);
    // The same tests again are refused: the drop did not take effect.
    assert!(
        drop_failing_tests(&got.spec, INSTALLED_LOG, &none)
            .unwrap_err()
            .contains("already dropped")
    );
}

#[test]
fn a_later_round_extends_the_single_k_expression() {
    let spec = installed_spec("%python_exec -m unittest discover -s pkg/tests");
    let first = drop_failing_tests(&spec, INSTALLED_LOG, &none).unwrap();
    assert!(first.spec.contains("-m pytest pkg/tests"), "{}", first.spec);
    let more = "FAIL: test_new (pkg.tests.test_io.IoTest.test_new)\n";
    let second = drop_failing_tests(&first.spec, more, &none).unwrap();
    assert_eq!(second.spec.matches("-k '").count(), 1, "{}", second.spec);
    assert!(
        second
            .spec
            .contains("and not (test_io and IoTest and test_new)'"),
        "{}",
        second.spec
    );
    assert!(second.spec.contains("test_roundtrip"), "{}", second.spec);
    assert_eq!(second.spec.matches("python_module pytest").count(), 1);
}

#[test]
fn an_installed_unittest_failure_under_a_pytest_check_uses_pytest_addopts() {
    let spec = installed_spec("%pytest --pyargs pkg");
    let got = drop_failing_tests(&spec, INSTALLED_LOG, &none).unwrap();
    assert!(got.spec.contains("%pytest --pyargs pkg"), "{}", got.spec);
    assert!(
        got.spec
            .contains("export PYTEST_ADDOPTS=\"${PYTEST_ADDOPTS:-} -k 'not (test_codec"),
        "{}",
        got.spec
    );
    assert!(got.spec.contains("python_module pytest"), "{}", got.spec);
    // A spec that already requires pytest gets no second one.
    let with = spec.replace(
        "python-rpm-macros",
        "python-rpm-macros\nBuildRequires:  %{python_module pytest}",
    );
    let got = drop_failing_tests(&with, INSTALLED_LOG, &none).unwrap();
    assert_eq!(got.spec.matches("python_module pytest").count(), 1);
}

#[test]
fn installed_unittest_failures_go_back_to_the_ladder_with_the_exact_reason() {
    for (check, needle) in [
        ("tox -e py3", "through `tox`"),
        ("make check", "through `make`"),
        ("echo nothing", "no unittest or pytest invocation"),
        ("%python_exec -m unittest -p x pkg", "no pytest equivalent"),
        (
            "%{python_expand $python -m unittest pkg}",
            "shell or macro syntax",
        ),
        ("%pytest -k 'not slow'", "own `-k`"),
    ] {
        let err = drop_failing_tests(&installed_spec(check), INSTALLED_LOG, &none).unwrap_err();
        assert!(err.contains(needle), "{check}: {err}");
        assert!(err.contains("not in the unpacked source"), "{check}: {err}");
    }
    // No spelling of pytest's BuildRequires known.
    let bare = "Name: x\n\n%prep\n%autosetup\n\n%check\n%python_exec -m unittest\n";
    assert!(
        drop_failing_tests(bare, INSTALLED_LOG, &none)
            .unwrap_err()
            .contains("BuildRequires")
    );
}

#[test]
fn source_and_installed_modules_are_dropped_each_their_own_way_in_one_round() {
    let log =
        "FAIL: test_a (pkg.tests.test_in.A.test_a)\nFAIL: test_b (pkg.tests.test_out.B.test_b)\n";
    let spec = installed_spec("%python_exec -m unittest pkg.tests");
    let have = |p: &str| p == "pkg/tests/test_in.py";
    let got = drop_failing_tests(&spec, log, &have).unwrap();
    assert_eq!(got.tests.len(), 2);
    assert!(got.spec.contains("sed -i -E '/^class A"), "{}", got.spec);
    assert!(
        got.spec.contains("not (test_out and B and test_b)"),
        "{}",
        got.spec
    );
    // The runner cannot take it: only the source one is dropped, the other is omitted.
    let spec = installed_spec("tox");
    let got = drop_failing_tests(&spec, log, &have).unwrap();
    assert_eq!(got.tests.len(), 1);
    assert_eq!(got.omitted, 1);
}
