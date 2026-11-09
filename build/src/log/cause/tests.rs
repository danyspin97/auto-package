use super::*;
use crate::log::PathMapper;

#[test]
fn a_cython_limited_api_error_gets_the_floor_recipe() {
    let log = log_from(&[
        "Executing(%build): /usr/bin/bash -e /var/tmp/rpm-tmp.abc",
        "tilelang_cython_wrapper.cpp:1:2: error: #error \"Cython 3.3 requires the Python Limited API version to be 3.9 or greater.\"",
        "error: Bad exit status from /var/tmp/rpm-tmp.abc (%build)",
    ]);
    let c = detect(&log, "tilelang", "").expect("recognised");
    assert_eq!(
        c.kind,
        CauseKind::KnownRecipe {
            id: "cython-limited-api"
        }
    );
    assert!(c.action.contains("cp39"), "{}", c.action);
    assert!(c.action.contains("sed -i"), "{}", c.action);
}

#[test]
fn a_cmake_licence_guard_points_to_convention_78() {
    let log = log_from(&[
        "Executing(%build): /usr/bin/bash -e /var/tmp/rpm-tmp.abc",
        "CMake Error at src/torchcodec/_core/CMakeLists.txt:13 (message):",
        "  set BUILD_AGAINST_ALL_FFMPEG_FROM_S3 or I_CONFIRM_THIS_IS_NOT_A_LICENSE_VIOLATION",
        "-- Configuring incomplete, errors occurred!",
        "error: Bad exit status from /var/tmp/rpm-tmp.abc (%build)",
    ]);
    let c = detect(&log, "torchcodec", "").expect("recognised");
    assert_eq!(
        c.kind,
        CauseKind::KnownRecipe {
            id: "cmake-licence-guard"
        }
    );
    assert!(
        c.action.contains("convention 5 (section 78)"),
        "{}",
        c.action
    );
}

#[test]
fn a_missing_cutlass_submodule_is_the_split_tree() {
    let log = log_from(&[
        "Executing(%check): /usr/bin/bash -e /var/tmp/rpm-tmp.abc",
        "    import cutlass.cute as cute",
        "ModuleNotFoundError: No module named 'cutlass.cute'",
        "error: Bad exit status from /var/tmp/rpm-tmp.abc (%check)",
    ]);
    let c = detect(&log, "tokenspeed_mla", "").expect("recognised");
    assert_eq!(
        c.kind,
        CauseKind::KnownRecipe {
            id: "cutlass-split-tree"
        }
    );
    assert!(c.action.contains("%{python_sitelib}"), "{}", c.action);
}

#[test]
fn a_file_named_like_a_network_word_is_not_network() {
    let msg = "FileNotFoundError: [Errno 2] No such file or directory: 'connect_fixture.csv'";
    assert_eq!(classify_message(msg), "missing_file");
    assert_eq!(
        classify_message("ModuleNotFoundError: No module named 'socket_helpers'"),
        "missing_module"
    );
    assert_eq!(
        classify_message("networkx.exception.NetworkXError: graph is empty"),
        "other"
    );
}

#[test]
fn real_network_failures_are_network() {
    for msg in [
        "ConnectionRefusedError: [Errno 111] Connection refused",
        "ConnectionResetError: [Errno 104] Connection reset by peer",
        "socket.timeout: timed out",
        "socket.gaierror: [Errno -2] Name or service not known",
        "[Errno -3] Temporary failure in name resolution",
        "ssl.SSLError: [SSL: CERTIFICATE_VERIFY_FAILED]",
        "httpx2.ConnectError: boom",
    ] {
        assert_eq!(classify_message(msg), "network", "{msg}");
    }
}

/// The Cython recipe's line starts past character 230, so its text is
/// cut off by the 300-character limit. The match must still be found, and
/// the later recipe's line must still be reachable.
#[test]
fn a_long_line_does_not_hide_the_recipes_after_it() {
    let cython = format!(
        "{}Cython 3.3 requires the Python Limited API version to be 3.8 or greater",
        "x".repeat(230)
    );
    assert!(cython.len() > 300);
    let haystack = [
        cython.as_str(),
        "CMake Error at CMakeLists.txt:3 (message):",
        "  set I_CONFIRM_THIS_IS_NOT_A_LICENSE_VIOLATION",
    ];
    let c = known_recipe(&haystack).expect("recognised");
    assert_eq!(
        c.kind,
        CauseKind::KnownRecipe {
            id: "cython-limited-api"
        }
    );
    assert!(c.evidence[0].chars().count() <= 300);

    // Without the Cython line, the licence guard further down is found.
    let haystack = [
        "CMake Error at CMakeLists.txt:3 (message):",
        "  set I_CONFIRM_THIS_IS_NOT_A_LICENSE_VIOLATION",
    ];
    let c = known_recipe(&haystack).expect("recognised");
    assert_eq!(
        c.kind,
        CauseKind::KnownRecipe {
            id: "cmake-licence-guard"
        }
    );
}

#[test]
fn truncated_match_rejects_short_fragments() {
    assert!(truncated_match("connectione...", "connectionerror"));
    assert!(!truncated_match("res...", "resolutionimpossible"));
    assert!(
        !truncated_match("res...", "resolutionimpossible")
            && classify_message("res...") != "network"
    );
}

fn log_from(lines: &[&str]) -> ExtractedLog {
    let text = lines
        .iter()
        .enumerate()
        .map(|(i, l)| format!("[{:>4}s] {l}", i))
        .collect::<Vec<_>>()
        .join("\n");
    crate::log::extract(&text, &PathMapper::with_sdist_root("accelerate-1.0"))
}

fn accelerate_log() -> ExtractedLog {
    log_from(&[
        "Executing(%check): /usr/bin/bash -e /var/tmp/rpm-tmp.abc",
        "+ pytest",
        "Traceback (most recent call last):",
        "  File \"/home/abuild/rpmbuild/BUILD/python-accelerate-1.0-build/accelerate-1.0/tests/test_big_modeling.py\", line 20, in <module>",
        "    from transformers import AutoModel",
        "ModuleNotFoundError: No module named 'transformers'",
        "error: Bad exit status from /var/tmp/rpm-tmp.abc (%check)",
    ])
}

/// The case that prompted this module.
#[test]
fn a_test_importing_an_undeclared_module_is_a_missing_build_requires() {
    let spec = "BuildRequires:  %{python_module pytest}\n";
    let c = detect(&accelerate_log(), "accelerate", spec).expect("recognised");
    assert_eq!(
        c.kind,
        CauseKind::MissingPythonModule {
            module: "transformers".into(),
            requirement: "transformers".into()
        }
    );
    assert!(
        c.action.contains("%{python_module transformers}"),
        "{}",
        c.action
    );
    assert!(
        c.action.contains("EVERY other third-party module"),
        "{}",
        c.action
    );
    assert!(
        c.evidence
            .iter()
            .any(|e| e.contains("No module named 'transformers'")),
        "{:?}",
        c.evidence
    );
    assert!(
        c.evidence
            .iter()
            .any(|e| e.contains("tests/test_big_modeling.py:20")),
        "{:?}",
        c.evidence
    );
    let rendered = c.render();
    assert!(rendered.starts_with("## Probable cause"), "{rendered}");
    assert!(
        rendered.contains("you do not need to read build.log"),
        "{rendered}"
    );
}

#[test]
fn a_module_the_spec_already_declares_points_at_resolution_not_a_new_line() {
    let spec = "BuildRequires:  %{python_module transformers >= 4.30}\n";
    let c = detect(&accelerate_log(), "accelerate", spec).unwrap();
    assert!(c.summary.contains("already lists"), "{}", c.summary);
    assert!(!c.action.starts_with("Add `BuildRequires"), "{}", c.action);
}

#[test]
fn import_names_map_to_their_distribution_names() {
    let log = log_from(&[
        "Executing(%check): /usr/bin/bash -e /var/tmp/x",
        "ModuleNotFoundError: No module named 'yaml'",
        "error: Bad exit status from /var/tmp/x (%check)",
    ]);
    match detect(&log, "foo", "").unwrap().kind {
        CauseKind::MissingPythonModule { requirement, .. } => assert_eq!(requirement, "PyYAML"),
        k => panic!("{k:?}"),
    }
}

#[test]
fn the_packages_own_module_failing_is_a_path_problem() {
    let log = log_from(&[
        "Executing(%check): /usr/bin/bash -e /var/tmp/x",
        "ModuleNotFoundError: No module named 'nvidia'",
        "error: Bad exit status from /var/tmp/x (%check)",
    ]);
    let c = detect(&log, "nvidia", "").unwrap();
    assert!(matches!(c.kind, CauseKind::OwnModuleNotImportable { .. }));
    assert!(c.action.contains("PYTHONPATH"));
}

#[test]
fn a_removed_stdlib_module_is_not_a_missing_package() {
    let log = log_from(&[
        "Executing(%build): /usr/bin/bash -e /var/tmp/x",
        "ModuleNotFoundError: No module named 'distutils'",
        "error: Bad exit status from /var/tmp/x (%build)",
    ]);
    let c = detect(&log, "foo", "").unwrap();
    assert!(matches!(c.kind, CauseKind::RemovedStdlibModule { .. }));
    assert!(c.action.contains("convention 116"));
}

#[test]
fn an_undefined_symbol_points_at_a_dependency_abi_not_a_missing_package() {
    let l = log_from(&[
        "OSError: /build/tilelang/lib/libtvm_runtime.so: undefined symbol: _ZN3tvm3ffi4json9StringifyERKN",
    ]);
    let cause = detect(&l, "tilelang", "").unwrap();
    match cause.kind {
        CauseKind::UndefinedSymbol { symbol } => assert!(symbol.starts_with("_ZN3tvm")),
        k => panic!("{k:?}"),
    }
    assert!(cause.action.contains("patch_dependencies"));
}

#[test]
fn missing_headers_commands_and_cmake_packages_are_recognised() {
    let h = log_from(&["foo.c:1:10: fatal error: libpq-fe.h: No such file or directory"]);
    match detect(&h, "", "").unwrap().kind {
        CauseKind::MissingHeader { header } => assert_eq!(header, "libpq-fe.h"),
        k => panic!("{k:?}"),
    }
    let c = log_from(&["/var/tmp/rpm-tmp.x: line 9: ninja: command not found"]);
    match detect(&c, "", "").unwrap().kind {
        CauseKind::MissingCommand { command } => assert_eq!(command, "ninja"),
        k => panic!("{k:?}"),
    }
    let p = log_from(&["FileNotFoundError: [Errno 2] No such file or directory: 'pg_config'"]);
    match detect(&p, "", "").unwrap().kind {
        CauseKind::MissingCommand { command } => assert_eq!(command, "pg_config"),
        k => panic!("{k:?}"),
    }
    let m = log_from(&[
        "CMake Error at CMakeLists.txt:12 (find_package):",
        "  Could not find a package configuration file provided by \"MLIR\" with any of",
    ]);
    match detect(&m, "", "").unwrap().kind {
        CauseKind::MissingCMakePackage { package } => assert_eq!(package, "MLIR"),
        k => panic!("{k:?}"),
    }
}

#[test]
fn an_unrecognised_failure_yields_no_cause() {
    let log = log_from(&[
        "Executing(%check): /usr/bin/bash -e /var/tmp/x",
        "AssertionError: 1 != 2",
        "error: Bad exit status from /var/tmp/x (%check)",
    ]);
    assert_eq!(detect(&log, "foo", ""), None);
}

/// python-accelerate's real %check: dozens of failures from four different
/// environment causes, which the agent had been peeling off one per build.
#[test]
fn many_failing_tests_become_one_digest_with_a_single_plan() {
    let mut lines = vec!["Executing(%check): /usr/bin/bash -e /var/tmp/rpm-tmp.abc".to_string()];
    for t in ["a", "b", "c"] {
        lines.push(format!(
            "FAILED src/accelerate/test_utils/scripts/test_sync.py::test_{t} - AssertionError"
        ));
    }
    for t in ["a", "b", "c", "d"] {
        lines.push(format!(
            "FAILED tests/test_cli.py::TpuConfigTester::test_{t} - FileNotFoundError: x"
        ));
    }
    lines.push(
        "FAILED tests/test_cli.py::ModelEstimatorTester::test_gated - httpx2.ConnectError".into(),
    );
    lines.push(
        "FAILED tests/test_utils.py::UtilsTester::test_fp32 - ModuleNotFoundError: triton".into(),
    );
    lines.push("error: Bad exit status from /var/tmp/rpm-tmp.abc (%check)".into());
    let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
    let log = log_from(&refs);
    let c = detect(&log, "accelerate", "").expect("digest");
    assert_eq!(c.kind, CauseKind::PytestFailures { failed: 9 });
    assert!(c.summary.contains("test ENVIRONMENT"), "{}", c.summary);
    assert!(c.action.contains("`%pytest tests/`"), "{}", c.action);
    assert!(c.action.contains("missing file or fixture"), "{}", c.action);
    assert!(c.action.contains("need network"), "{}", c.action);
    assert!(
        c.action.contains("EVERY missing third-party module"),
        "{}",
        c.action
    );
    assert!(
        c.evidence
            .iter()
            .any(|e| e.starts_with("tests/test_cli.py: 5 failed")),
        "{:?}",
        c.evidence
    );
}

/// pytest truncates its one-line failure summary to terminal width, so a
/// long exception name often arrives as `FileN...` rather than in full.
#[test]
fn truncated_exception_names_still_classify() {
    let mut lines = vec!["Executing(%check): /usr/bin/bash -e /var/tmp/rpm-tmp.abc".to_string()];
    for (i, tail) in [
        "FileN...",
        "FileNo...",
        "FileNotFoun...",
        "FileNotFoundErro...",
    ]
    .into_iter()
    .enumerate()
    {
        lines.push(format!(
            "FAILED tests/test_cli.py::Tester::test_{i} - {tail}"
        ));
    }
    lines.push("FAILED tests/test_cli.py::Tester::test_net - httpx2.ConnectEr...".into());
    lines.push("error: Bad exit status from /var/tmp/rpm-tmp.abc (%check)".into());
    let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
    let log = log_from(&refs);
    let c = detect(&log, "accelerate", "").expect("digest");
    assert!(
        c.evidence
            .iter()
            .any(|e| e.contains("4 missing file/fixture") && e.contains("1 need network")),
        "{:?}",
        c.evidence
    );
}

#[test]
fn a_handful_of_failures_is_not_a_digest() {
    let log = log_from(&[
        "Executing(%check): /usr/bin/bash -e /var/tmp/x",
        "FAILED tests/test_a.py::test_x - AssertionError",
        "FAILED tests/test_a.py::test_y - AssertionError",
        "error: Bad exit status from /var/tmp/x (%check)",
    ]);
    assert_eq!(detect(&log, "foo", ""), None);
}
