use super::patch_rules_content::*;

const DIFF: &str =
    "--- foo-1.0/src/foo.c\n+++ foo-1.0/src/foo.c\n@@ -1 +1 @@\n-int x;\n+int x = 0;\n";

fn problems_of(text: &str) -> Vec<String> {
    source_only_problems(&patch_changes(text))
}

/// A one-hunk patch of `file` that removes `removed` and adds `added`.
fn build_edit(file: &str, removed: &[&str], added: &[&str]) -> String {
    let (r, a) = (removed.len(), added.len());
    let mut text = format!("--- foo-1.0/{file}\n+++ foo-1.0/{file}\n@@ -1,{r} +1,{a} @@\n");
    for r in removed {
        text.push_str(&format!("-{r}\n"));
    }
    for a in added {
        text.push_str(&format!("+{a}\n"));
    }
    text
}

/// A one-hunk patch of the source `file` that adds `added` (with no removal).
fn source_add(file: &str, added: &str) -> String {
    format!("--- foo-1.0/{file}\n+++ foo-1.0/{file}\n@@ -1 +1,2 @@\n x\n+{added}\n")
}

/// A one-hunk patch of `file` that removes `removed` and adds `added`, as the
/// removal and the addition of one line.
fn replace_line(file: &str, removed: &str, added: &str) -> String {
    format!("--- foo-1.0/{file}\n+++ foo-1.0/{file}\n@@ -1 +1 @@\n-{removed}\n+{added}\n")
}

#[test]
fn a_source_fix_with_its_diff_passes_the_source_rule() {
    assert!(problems_of(DIFF).is_empty());
}

#[test]
fn or_true_is_refused_in_every_spelling() {
    for line in [
        "test-run || true",
        "test-run ||true",
        "test-run ||  true",
        "test-run || :",
        "test-run || exit 0",
        "test-run; true",
        "set +e",
    ] {
        let text = build_edit("Makefile.in", &[], &[line]);
        assert!(
            problems_of(&text)
                .iter()
                .any(|p| p.contains("|| true") || p.contains("hides")),
            "{line}"
        );
    }
}

#[test]
fn a_hiding_form_in_a_shell_context_is_refused_wherever_it_ends_a_command() {
    for (file, line) in [
        ("tests/run.sh", "test-run || exit 0"),
        ("tests/run.sh", "test-run || :"),
        ("scripts/check.in", "test-run;true"),
        ("foo.spec", "make check || true"),
        ("src/a.c", "\ttest-run || true"),
        ("Makefile", "set +e"),
        ("tests/run.sh", "test-run || true # flaky"),
    ] {
        let text = build_edit(file, &[], &[line]);
        assert!(
            problems_of(&text).iter().any(|p| p.contains("hides")),
            "{file}: {line}"
        );
    }
}

#[test]
fn a_hiding_form_is_not_read_in_c_rust_or_python_arithmetic_and_names() {
    // In C, Rust and Python `||` and `;` are operators and a name is a name.
    for (file, line) in [
        ("src/a.c", "x = 1; true_count++;"),
        ("src/a.c", "if (ok || true) {"),
        ("src/a.c", "int offset = a + element_size;"),
        ("src/a.c", "if (ok || exit_code == 0) return;"),
        ("src/lib.rs", "let b = a || true;"),
        ("src/lib.rs", "let set_e = offset + element_size;"),
        ("pkg/mod.py", "ok = a or True"),
        ("pkg/mod.py", "x = 1; true_count = 2"),
    ] {
        let text = build_edit(file, &[], &[line]);
        assert!(
            !problems_of(&text).iter().any(|p| p.contains("hides")),
            "{file}: {line}"
        );
    }
    // The same words in a shell script are a command that does hide.
    let shell = build_edit("tests/a.sh", &[], &["x=1; true_count++"]);
    assert!(!problems_of(&shell).iter().any(|p| p.contains("hides")));
    let shell_set = build_edit("tests/a.sh", &[], &["x=offset + element_size"]);
    assert!(!problems_of(&shell_set).iter().any(|p| p.contains("hides")));
}

#[test]
fn a_make_recipe_that_ignores_its_failure_is_refused() {
    let text =
        "--- foo-1.0/Makefile.am\n+++ foo-1.0/Makefile.am\n@@ -1 +1,2 @@\n x\n+\t-$(MAKE) check\n";
    assert!(
        problems_of(text)
            .iter()
            .any(|p| p.contains("ignores its failure"))
    );
}

#[test]
fn wno_error_needs_a_comment_on_its_line() {
    let bare = "--- foo-1.0/src/a.c\n+++ foo-1.0/src/a.c\n@@ -1 +1 @@\n-x\n+ -Wno-error\n";
    assert!(problems_of(bare).iter().any(|p| p.contains("-Wno-error")));
    let explained = "--- foo-1.0/src/a.c\n+++ foo-1.0/src/a.c\n@@ -1 +1 @@\n-x\n+ -Wno-error // gcc 15 warns on the old cast\n";
    assert!(problems_of(explained).is_empty());
}

#[test]
fn wno_error_may_be_explained_on_the_line_above_it() {
    // A comment line of the new file just above the flag (a context line here).
    let above = "--- foo-1.0/Makefile.am\n+++ foo-1.0/Makefile.am\n@@ -1 +1,2 @@\n # gcc 15 warns on the old cast\n+ -Wno-error\n";
    assert!(problems_of(above).is_empty(), "{:?}", problems_of(above));
    // An added comment line above it explains it too.
    let added = "--- foo-1.0/Makefile.am\n+++ foo-1.0/Makefile.am\n@@ -1 +1,3 @@\n+# gcc 15 warns on the old cast\n+ -Wno-error\n x\n";
    assert!(problems_of(added).is_empty(), "{:?}", problems_of(added));
    // A comment two lines up does not count: the line above must carry it.
    let two_up = "--- foo-1.0/Makefile.am\n+++ foo-1.0/Makefile.am\n@@ -1,2 +1,3 @@\n # gcc 15 warns\n int a;\n+ -Wno-error\n";
    assert!(problems_of(two_up).iter().any(|p| p.contains("-Wno-error")));
}

#[test]
fn a_hunk_starts_with_no_line_above_it() {
    // The comment is in the previous hunk, so it does not explain this flag.
    let text = "--- foo-1.0/Makefile.am\n+++ foo-1.0/Makefile.am\n@@ -1 +1 @@\n # why\n@@ -9 +9,2 @@\n+ -Wno-error\n";
    assert!(problems_of(text).iter().any(|p| p.contains("-Wno-error")));
}

#[test]
fn a_deleted_test_file_or_a_removed_test_is_refused() {
    let deleted = "--- foo-1.0/tests/test_a.py\n+++ /dev/null\n@@ -1 +0,0 @@\n-x\n";
    assert!(problems_of(deleted).iter().any(|p| p.contains("test_a.py")));
    let removed =
        "--- foo-1.0/src/lib.rs\n+++ foo-1.0/src/lib.rs\n@@ -1 +1 @@\n-#[test]\n+// gone\n";
    assert!(
        problems_of(removed)
            .iter()
            .any(|p| p.contains("removes a test"))
    );
}

#[test]
fn a_renamed_or_deleted_test_file_is_refused_by_git_headers() {
    let renamed = "diff --git a/tests/test_a.py b/tests/test_b.py\nsimilarity index 100%\nrename from tests/test_a.py\nrename to tests/test_b.py\n";
    assert!(
        problems_of(renamed)
            .iter()
            .any(|p| p.contains("tests/test_a.py"))
    );
    let deleted =
        "diff --git a/tests/x.py b/tests/x.py\ndeleted file mode 100644\nindex abc..0000\n";
    assert!(
        problems_of(deleted)
            .iter()
            .any(|p| p.contains("tests/x.py"))
    );
    // A rename of a source file is not a test.
    let source = "diff --git a/src/a.c b/src/b.c\nrename from src/a.c\nrename to src/b.c\n";
    assert!(problems_of(source).is_empty());
}

#[test]
fn a_test_renamed_inside_one_line_is_a_fix_and_a_removed_test_is_not() {
    let fixed = replace_line("tests/t.py", "def test_foo():", "def test_foo_bar():");
    assert!(problems_of(&fixed).is_empty(), "{:?}", problems_of(&fixed));
    let removed = replace_line("tests/t.py", "def test_foo():", "def check_foo():");
    assert!(
        problems_of(&removed)
            .iter()
            .any(|p| p.contains("removes a test"))
    );
    // A `def test_` in a source file is not a test marker: a helper of that name is not a test.
    let helper = replace_line("src/t.py", "def test_foo():", "def check_foo():");
    assert!(problems_of(&helper).is_empty());
    // A test marker that moved to another line of the same function is not a removal.
    let rust_moved = "--- foo-1.0/src/lib.rs\n+++ foo-1.0/src/lib.rs\n@@ -1,2 +1,2 @@\n-#[test]\n fn a() {}\n+#[test]\n";
    assert!(
        problems_of(rust_moved).is_empty(),
        "{:?}",
        problems_of(rust_moved)
    );
}

#[test]
fn the_newer_test_markers_are_removals_too() {
    for line in [
        "TEST_F(Foo, Bar) {",
        "TEST_P(Foo, Bar) {",
        "BOOST_AUTO_TEST_CASE(x) {",
        "#[tokio::test]",
    ] {
        let text = build_edit("src/lib.c", &[line], &["// gone"]);
        assert!(
            problems_of(&text)
                .iter()
                .any(|p| p.contains("removes a test")),
            "{line}"
        );
    }
    // `it(`, `describe(` and `class Test` are test markers in a test file only.
    for line in [
        "  it('works', () => {",
        "describe('x', () => {",
        "class TestFoo {",
    ] {
        let text = build_edit("tests/lib.js", &[line], &["// gone"]);
        assert!(
            problems_of(&text)
                .iter()
                .any(|p| p.contains("removes a test")),
            "{line}"
        );
        let source = build_edit("src/lib.js", &[line], &["// gone"]);
        assert!(problems_of(&source).is_empty(), "{line}");
    }
    // A word that only ends in `it(` is not a test marker.
    let text = build_edit("tests/lib.js", &["  split('a')"], &["// gone"]);
    assert!(problems_of(&text).is_empty());
}

#[test]
fn one_added_test_answers_one_removed_test() {
    // Two removed `#[test]` lines and one added one: one of the removals is refused.
    let two_for_one = build_edit("src/lib.rs", &["#[test]", "#[test]"], &["#[test]"]);
    assert_eq!(
        problems_of(&two_for_one)
            .iter()
            .filter(|p| p.contains("removes a test"))
            .count(),
        1
    );
    // A removed test with the same kind added back is not refused.
    let moved = build_edit("src/lib.rs", &["#[test]"], &["#[test]"]);
    assert!(problems_of(&moved).is_empty());
}

#[test]
fn a_typo_fixed_in_a_test_name_is_not_a_removal() {
    let fixed = replace_line("tests/t.py", "def test_fooo():", "def test_foo():");
    assert!(problems_of(&fixed).is_empty(), "{:?}", problems_of(&fixed));
    // A removed test with nothing added back is a removal.
    let gone = build_edit("tests/t.py", &["def test_fooo():"], &[]);
    assert!(
        problems_of(&gone)
            .iter()
            .any(|p| p.contains("removes a test"))
    );
    // A test of another kind is not an answer to a removed one.
    let other_kind = replace_line("tests/t.py", "def test_fooo():", "def check_foo():");
    assert!(
        problems_of(&other_kind)
            .iter()
            .any(|p| p.contains("removes a test"))
    );
}

#[test]
fn a_skip_added_to_a_test_source_is_refused_and_one_in_a_source_is_not() {
    for line in [
        "@pytest.mark.skip(reason='x')",
        "pytest.skip('x')",
        "#[ignore]",
        "GTEST_SKIP();",
        "t.Skip(\"x\")",
        "@unittest.skip('x')",
    ] {
        let in_test = source_add("tests/test_a.py", line);
        assert!(
            problems_of(&in_test)
                .iter()
                .any(|p| p.contains("adds a skip")),
            "{line}"
        );
    }
    assert!(problems_of(&source_add("src/a.py", "@pytest.mark.skip")).is_empty());
}

#[test]
fn test_directories_and_names_are_test_sources() {
    for path in [
        "t/a.c",
        "__tests__/a.js",
        "spec/a.rb",
        "unittests/a.c",
        "src/foo.test.js",
        "src/foo.spec.ts",
        "pkg/FooTest.java",
    ] {
        assert!(is_test_path(path), "{path}");
        let text = source_add(path, "@pytest.mark.skip");
        assert!(
            problems_of(&text).iter().any(|p| p.contains("adds a skip")),
            "{path}"
        );
    }
    assert!(!is_test_path("src/latest.c"));
}

#[test]
fn a_build_change_that_removes_a_test_or_check_step_is_refused() {
    let refused = [
        ("CMakeLists.txt", "add_test(NAME unit COMMAND foo)"),
        ("CMakeLists.txt", "enable_testing()"),
        ("CMakeLists.txt", "  add_subdirectory(tests)"),
        ("meson.build", "subdir('tests')"),
        ("meson.build", "test('unit', exe)"),
        ("Makefile.am", "check-local: check-foo"),
        ("Makefile.in", "check: all"),
        ("Makefile.in", "test-suite:"),
    ];
    for (file, line) in refused {
        let problems = problems_of(&build_edit(file, &[line], &[]));
        assert!(
            problems.iter().any(|p| p.contains("test or check step")),
            "{line}: {problems:?}"
        );
    }
}

#[test]
fn a_build_change_that_turns_a_skip_flag_on_is_refused() {
    let refused = [
        "set(BUILD_TESTING OFF)",
        "-DSKIP_TESTS=ON",
        "option('tests', type: 'boolean', value: false)",
        "-DDISABLE_CHECKS=1",
        "-DENABLE_TESTS=OFF",
        "--disable-tests",
        "-Dtests=false",
        "option(BUILD_TESTING \"build tests\" OFF)",
        "-DSKIP_BROKEN_PLUGIN=ON",
        "option(BUILD_PLUGIN \"build the plugin\" OFF)",
        "-DWITH_FOO=OFF",
        "--without-foo",
        "-DWITH_GUI=OFF",
        "option('build_docs', type: 'boolean', value: false)",
    ];
    for line in refused {
        let problems = problems_of(&build_edit("CMakeLists.txt", &[], &[line]));
        assert!(
            problems.iter().any(|p| p.contains("skip flag")),
            "{line}: {problems:?}"
        );
    }
}

#[test]
fn a_skip_flag_with_a_comment_on_its_line_or_above_is_explained() {
    let above = build_edit(
        "CMakeLists.txt",
        &[],
        &[
            "# the plugin crashes on load, see the upstream bug",
            "-DSKIP_BROKEN_PLUGIN=ON",
        ],
    );
    assert!(problems_of(&above).is_empty(), "{:?}", problems_of(&above));
    let same_line = build_edit("CMakeLists.txt", &[], &["-DWITH_GUI=OFF # no display here"]);
    assert!(
        problems_of(&same_line).is_empty(),
        "{:?}",
        problems_of(&same_line)
    );
    let bare = build_edit("CMakeLists.txt", &[], &["-DSKIP_BROKEN_PLUGIN=ON"]);
    assert!(problems_of(&bare).iter().any(|p| p.contains("skip flag")));
}

#[test]
fn an_autoconf_check_is_not_a_switch_off() {
    // `have_m=no` and the `no` of a message are what a check found, not a feature off.
    for (file, line) in [
        ("configure.ac", "AC_CHECK_LIB([m], [cos], , [have_m=no])"),
        (
            "configure.ac",
            "AC_MSG_CHECKING(whether the test helper is needed, no)",
        ),
        ("Makefile.am", "AM_CHECK_PYTHON(, no)"),
    ] {
        let problems = problems_of(&build_edit(file, &[], &[line]));
        assert!(problems.is_empty(), "{line}: {problems:?}");
    }
    // An autoconf option that is switched off is still refused.
    let option = build_edit(
        "configure.ac",
        &[],
        &["AC_ARG_ENABLE([tests], [], [enable_tests=no])"],
    );
    assert!(problems_of(&option).iter().any(|p| p.contains("skip flag")));
}

#[test]
fn a_build_line_that_only_mentions_a_skip_or_a_test_is_not_a_skip_flag() {
    let passes = [
        // A comment says nothing about the build.
        "# set(BUILD_TESTING OFF) was tried, it is not used",
        "# ignore foo",
        "// BUILD_TESTING OFF in upstream's note",
        // A Qt flag: `disable` is not about tests.
        "-DQT_DISABLE_DEPRECATED_BEFORE=0x060000",
        // A skip flag switched off, which does not skip anything.
        "-DSKIP_TESTS=OFF",
        // A value that is not a flag's on or off.
        "-DBUILD_DIR=nothing",
    ];
    for line in passes {
        assert!(
            problems_of(&build_edit("CMakeLists.txt", &[], &[line])).is_empty(),
            "{line} is not a test skip"
        );
    }
}

#[test]
fn a_build_change_that_removes_or_adds_something_else_passes() {
    // Removals that are not a test or check step: a dependency, an example, a
    // flag that is not about tests.
    let removals = [
        ("CMakeLists.txt", "find_package(OldDep REQUIRED)"),
        ("meson.build", "dependency('old')"),
        ("Makefile.in", "CHECK_FLAGS := -v"),
        ("Makefile.am", "SUBDIRS = src"),
    ];
    for (file, line) in removals {
        assert!(
            problems_of(&build_edit(file, &[line], &[])).is_empty(),
            "{line} is a legitimate removal"
        );
    }
    // Additions: a real dependency, a new test, a feature switched off that is not a
    // flag of a skip or a build option (`option_x`), and a feature switched on.
    let additions = [
        "find_package(Foo REQUIRED)",
        "add_test(NAME new COMMAND foo)",
        "option_x = false",
        "-DENABLE_FOO=ON",
    ];
    for line in additions {
        assert!(
            problems_of(&build_edit("CMakeLists.txt", &[], &[line])).is_empty(),
            "{line} is allowed"
        );
    }
}

#[test]
fn a_patch_whose_diff_has_a_removed_dash_line_is_read_by_its_hunk() {
    // A removed `-- x` (an SQL comment) and an added `++ y` are lines of a hunk, not a
    // file header: the file stays foo-1.0/f.sql.
    let text = "--- foo-1.0/f.sql\n+++ foo-1.0/f.sql\n@@ -1,1 +1,1 @@\n--- x\n+++ y\n";
    let changes = patch_changes(text);
    assert_eq!(
        changes.removed,
        vec![("foo-1.0/f.sql".to_string(), "-- x".to_string())]
    );
    assert_eq!(changes.added.len(), 1);
    assert_eq!(changes.added[0].file, "foo-1.0/f.sql");
    assert_eq!(changes.added[0].text, "++ y");
}

#[test]
fn a_build_change_that_drops_a_directory_or_a_test_list_is_refused() {
    let dropped = build_edit("CMakeLists.txt", &["add_subdirectory(examples)"], &[]);
    assert!(
        problems_of(&dropped)
            .iter()
            .any(|p| p.contains("removes the subdirectory examples"))
    );
    // A directory removed and added back in the same change is not dropped.
    let moved = build_edit(
        "CMakeLists.txt",
        &["add_subdirectory(examples)"],
        &["add_subdirectory(examples)"],
    );
    assert!(problems_of(&moved).is_empty(), "{:?}", problems_of(&moved));
    let tests = build_edit("meson.build", &["subdir('tests')"], &[]);
    assert!(
        problems_of(&tests)
            .iter()
            .any(|p| p.contains("test or check step"))
    );
    let emptied = build_edit("Makefile.am", &["TESTS = a.sh b.sh"], &["TESTS ="]);
    assert!(
        problems_of(&emptied)
            .iter()
            .any(|p| p.contains("empties TESTS"))
    );
    let dropped_word = build_edit("Makefile.am", &["SUBDIRS = src tests"], &["SUBDIRS = src"]);
    assert!(
        problems_of(&dropped_word)
            .iter()
            .any(|p| p.contains("drops the test or check tests"))
    );
    // Words added to a list, or a list that keeps its tests, pass.
    let grown = build_edit(
        "Makefile.am",
        &["SUBDIRS = src tests"],
        &["SUBDIRS = src tests lib"],
    );
    assert!(problems_of(&grown).is_empty(), "{:?}", problems_of(&grown));
}
