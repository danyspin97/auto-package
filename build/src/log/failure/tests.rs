//! Tests of the failure locator and of the genre each failure gets.

use super::super::phase::segment;
use super::*;

#[test]
fn error_count_summary_is_not_a_post_build_problem() {
    assert!(!POST_BUILD_PROBLEM.is_match("Summary: 0 ERRORS, 2 warnings"));
    assert!(POST_BUILD_PROBLEM.is_match("ERROR: broken"));
    assert!(POST_BUILD_PROBLEM.is_match("ERROR: boom"));
    assert!(POST_BUILD_PROBLEM.is_match("ERROR_FOO happened"));
    assert!(POST_BUILD_PROBLEM.is_match("ERRORS"));
    assert!(!POST_BUILD_PROBLEM.is_match("0 ERRORS"));
}

fn mapper() -> PathMapper {
    PathMapper::with_sdist_root("glances-4.3.0")
}

fn run(lines: &[&str]) -> Failure {
    let spans = segment(lines);
    locate(lines, &spans, &mapper()).expect("a failure")
}

/// Iteration 2, verbatim in shape: a shell error with xtrace context.
#[test]
fn shell_error_keeps_the_command_that_ran() {
    let f = run(&[
        "Executing(%check): /usr/bin/bash -e /var/tmp/rpm-tmp.Ru643O",
        "+ umask 022",
        "+ cd /home/abuild/rpmbuild/BUILD/python-glances-4.3.0-build",
        "+ cd glances-4.3.0",
        "+ CHOOSE:",
        "/var/tmp/rpm-tmp.Ru643O: line 33: CHOOSE:: command not found",
        "error: Bad exit status from /var/tmp/rpm-tmp.Ru643O (%check)",
    ]);
    assert_eq!(f.genre, Genre::ShellError);
    assert_eq!(f.phase, Phase::Section("%check".into()));
    // The xtrace line is what tells the model the placeholder was executed.
    assert!(f.excerpt.contains("+ CHOOSE:"));
    assert!(f.excerpt.contains("command not found"));
}

/// duckdb 2.0.0 (run 1, build 011): a missing header in a C++ compile. Not
/// a shell error, not a traceback: the genre must say it is a C/C++ compile.
#[test]
fn a_cpp_missing_header_is_a_compile_failure() {
    let f = run(&[
        "Executing(%build): /usr/bin/bash -e /var/tmp/rpm-tmp.K2CoLj",
        "[ 40%] Building CXX object src/CMakeFiles/duckdb.dir/a.cpp.o",
        "In file included from /home/abuild/rpmbuild/BUILD/duckdb-2.0.0-build/duckdb-2.0.0/src/a.cpp:1:",
        "/home/abuild/rpmbuild/BUILD/duckdb-2.0.0-build/duckdb-2.0.0/build/from_variant.cpp:10:10: fatal error: yyjson_memory.hpp: No such file or directory",
        "   10 | #include \"yyjson_memory.hpp\"",
        "make[2]: *** [CMakeFiles/duckdb.dir/build.make:79: a.o] Error 1",
        "error: Bad exit status from /var/tmp/rpm-tmp.K2CoLj (%build)",
    ]);
    assert_eq!(f.genre, Genre::CCppCompile);
    assert_eq!(f.genre.as_str(), "c-cpp-compile");
    assert_eq!(f.phase, Phase::Section("%build".into()));
    assert!(f.excerpt.contains("fatal error: yyjson_memory.hpp"));
}

/// A gcc `error:` with a location, and the first of several: the later
/// one is a consequence, so the excerpt starts at the first.
#[test]
fn the_first_compiler_error_is_quoted() {
    let f = run(&[
        "Executing(%build): /usr/bin/bash -e /var/tmp/rpm-tmp.Q",
        "/home/abuild/rpmbuild/BUILD/pkg-1.0-build/pkg-1.0/src/x.cc:3:5: error: 'foo' was not declared in this scope",
        "/home/abuild/rpmbuild/BUILD/pkg-1.0-build/pkg-1.0/src/y.cc:9:1: error: expected ';'",
        "error: Bad exit status from /var/tmp/rpm-tmp.Q (%build)",
    ]);
    assert_eq!(f.genre, Genre::CCppCompile);
    assert!(f.excerpt.contains("src/x.cc:3:5: error"));
}

/// Iteration 3: no `error:` line, no `Bad exit status`, diagnosis in
/// post-build. The case a naive anchor misses entirely.
#[test]
fn post_build_failure_is_found_without_rpm_error_vocabulary() {
    let lines = &[
        "Executing(%install): /usr/bin/bash -e /var/tmp/rpm-tmp.aaa",
        "copying glances/plugins/foo.py",
        "... running 00-check-install-rpms",
        "/var/tmp/rpm-tmp.KodKmb: line 2: update-alternatives: command not found",
        "warning: %post(python313-glances-4.3.0-0.noarch) scriptlet failed, exit status 127",
        "... running 50-check-filelist",
        "python313-glances-4.3.0-0.noarch.rpm: directories not owned by a package:",
        " - /etc/alternatives",
    ];
    assert!(
        !lines.iter().any(|l| l.starts_with("error: ")),
        "fixture must not contain rpm error vocabulary"
    );

    let f = run(lines);
    assert_eq!(f.genre, Genre::PostBuildCheck);
    assert_eq!(f.phase, Phase::PostBuild);
    assert!(f.excerpt.contains("/etc/alternatives"));
    // …and it names which check objected.
    assert!(f.excerpt.contains("check-install-rpms") || f.excerpt.contains("check-filelist"));
}

#[test]
fn traceback_keeps_our_source_and_elides_plumbing() {
    let mut lines = vec!["Executing(%install): /usr/bin/bash -e /var/tmp/rpm-tmp.Asoqjo"];
    lines.push("Traceback (most recent call last):");
    lines.push(
        r#"  File "/home/abuild/rpmbuild/BUILD/python-glances-4.3.0-build/glances-4.3.0/setup.py", line 1, in <module>"#,
    );
    lines.push("    setup()");
    // Ten frames of interpreter plumbing.
    for _ in 0..10 {
        lines.push(
            r#"  File "/usr/lib/python3.14/site-packages/setuptools/_distutils/dist.py", line 1002, in run_command"#,
        );
        lines.push("    cmd_obj.run()");
    }
    lines.push(
        r#"  File "/home/abuild/rpmbuild/BUILD/python-glances-4.3.0-build/glances-4.3.0/fastentrypoints.py", line 62, in get_args"#,
    );
    lines.push("    spec = str(dist.as_requirement())");
    lines.push("AttributeError: 'PathDistribution' object has no attribute 'as_requirement'");
    lines.push("error: Bad exit status from /var/tmp/rpm-tmp.Asoqjo (%install)");

    let f = run(&lines);
    assert_eq!(f.genre, Genre::PythonTraceback);
    assert_eq!(f.phase, Phase::Section("%install".into()));

    // Both of our files survive, wherever they sat in the stack.
    assert!(f.excerpt.contains("setup.py"), "entry point dropped");
    assert!(f.excerpt.contains("fastentrypoints.py"), "culprit dropped");
    assert!(f.excerpt.contains("AttributeError"));
    // The plumbing does not.
    assert!(f.elided > 0, "nothing was elided");
    assert!(f.excerpt.contains("elided"));
    assert!(f.excerpt.lines().count() < lines.len());
}

#[test]
fn short_tracebacks_are_left_intact() {
    // Collapsing a three-frame traceback saves nothing and loses context.
    let lines = &[
        "Executing(%build): /usr/bin/bash -e /var/tmp/rpm-tmp.b",
        "Traceback (most recent call last):",
        r#"  File "/usr/lib/python3.14/site-packages/a.py", line 1, in x"#,
        "    boom()",
        "ValueError: nope",
        "error: Bad exit status from /var/tmp/rpm-tmp.b (%build)",
    ];
    let f = run(lines);
    assert_eq!(f.genre, Genre::PythonTraceback);
    assert_eq!(f.elided, 0);
    assert!(!f.excerpt.contains("elided"));
}

/// Real shape from `python-duckdb`: rpm's own unpackaged-file check runs
/// right after the last scriptlet section (`%license` here), never
/// inside an OBS `... running NN-check-*` span — so there is no
/// `Phase::PostBuild` for `post_build_failure` to find, and this used to
/// fall through `bare_error_line` into `Genre::Unknown` instead of being
/// recognised as the same kind of failure `post_build_failure` handles.
#[test]
fn unpackaged_files_outside_a_postbuild_span_is_still_post_build_check() {
    let lines = &[
        "Executing(%license): /usr/bin/bash -e /var/tmp/rpm-tmp.vgFDg8",
        "+ cp -pr /build/duckdb-1.5.6/LICENSE /buildroot/usr/share/licenses/python313-duckdb",
        "+ exit 0",
        "Provides: python313-duckdb = 1.5.6-3.1",
        "Checking for unpackaged file(s): /usr/lib/rpm/check-files /buildroot",
        "error: Installed (but unpackaged) file(s) found:",
        "   /usr/lib/python3.13/site-packages/_duckdb.cpython-313-x86_64-linux-gnu.so",
    ];
    assert!(
        !lines.iter().any(|l| l.starts_with("... running")),
        "fixture must not contain an OBS check-scriptlet marker"
    );

    let f = run(lines);
    assert_eq!(f.genre, Genre::PostBuildCheck);
    assert!(f.excerpt.contains("Installed (but unpackaged)"));
    assert!(f.excerpt.contains("_duckdb.cpython"));
}

#[test]
fn build_chatter_is_dropped_from_unrecognised_failures() {
    let lines = &[
        "Executing(%install): /usr/bin/bash -e /var/tmp/rpm-tmp.c",
        "copying glances/plugins/a.py -> build/lib",
        "byte-compiling /x/y.py to y.pyc",
        "creating build/lib/glances",
        "something genuinely odd happened",
        "error: Bad exit status from /var/tmp/rpm-tmp.c (%install)",
    ];
    let f = run(lines);
    assert_eq!(f.genre, Genre::Unknown);
    assert_eq!(f.elided, 3, "three chatter lines should have gone");
    assert!(f.excerpt.contains("genuinely odd"));
    assert!(!f.excerpt.contains("byte-compiling"));
}

/// The `Bad exit status` line names a script whose `Executing` line comes
/// after it: the section span starts past the error, and must not be
/// sliced as an inverted range.
#[test]
fn a_section_that_starts_after_the_bad_exit_line_does_not_panic() {
    let lines = &[
        "error: Bad exit status from /var/tmp/rpm-tmp.Z (%check)",
        "something printed after the error",
        "Executing(%check): /usr/bin/bash -e /var/tmp/rpm-tmp.Z",
        "+ cd glances-4.3.0",
        "/var/tmp/rpm-tmp.Z: line 3: CHOOSE:: command not found",
    ];
    let f = run(lines);
    assert!(f.excerpt.contains("Bad exit status"));
    assert_eq!(f.located_by, BY_RPM_REPORT);
}

#[test]
fn excerpt_is_capped() {
    let mut lines = vec!["Executing(%install): /usr/bin/bash -e /var/tmp/rpm-tmp.d"];
    for i in 0..500 {
        lines.push(Box::leak(format!("unrecognised line {i}").into_boxed_str()));
    }
    lines.push("error: Bad exit status from /var/tmp/rpm-tmp.d (%install)");
    let f = run(&lines);
    assert!(f.excerpt.lines().count() <= MAX_EXCERPT_LINES);
    // The cap keeps the end, where the verdict is.
    assert!(f.excerpt.contains("Bad exit status"));
}

#[test]
fn noise_filter_matches_the_real_chatter() {
    for l in [
        "copying glances/plugins/foo.py -> build/lib/glances/plugins",
        "byte-compiling /home/abuild/x.py to x.cpython-314.pyc",
        "creating build/lib/glances",
        "calling /usr/lib/rpm/brp-suse.d/brp-25-symlink",
    ] {
        assert!(is_noise(l), "should be noise: {l}");
    }
    for l in [
        "AttributeError: boom",
        " - /etc/alternatives",
        "error: Bad exit status from /x (%install)",
    ] {
        assert!(!is_noise(l), "should not be noise: {l}");
    }
}
