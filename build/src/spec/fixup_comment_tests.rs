//! A fixup an edit adds to a build phase needs a comment that says why (convention 40).

use crate::spec::unexplained_fixups;

/// An origin spec with a `%prep`, a `%build` and a `%check` and no fixup.
const ORIGIN: &str = "\
%prep
%autosetup -p1 -n ExifRead-%{version}

%build
%pyproject_wheel

%check
%pytest
";

/// `ORIGIN` with `line` added to `%prep`, after the `%autosetup`.
fn with_prep_line(line: &str) -> String {
    ORIGIN.replacen(
        "%autosetup -p1 -n ExifRead-%{version}\n",
        &format!("%autosetup -p1 -n ExifRead-%{{version}}\n{line}\n"),
        1,
    )
}

#[test]
fn the_exifread_sed_without_a_comment_is_a_problem() {
    let after = with_prep_line("sed -i '1{/^#!/d}' exifread/cli.py");
    let problems = unexplained_fixups(ORIGIN, &after, "python-ExifRead");
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].contains("line 3"), "{problems:?}");
    assert!(
        problems[0].contains("sed -i '1{/^#!/d}' exifread/cli.py"),
        "{problems:?}"
    );
    assert!(
        problems[0].contains("no comment directly above"),
        "{problems:?}"
    );
}

#[test]
fn the_same_sed_with_a_comment_above_it_is_accepted() {
    let after = with_prep_line(
        "# the script starts with a shebang the wheel does not need\n\
         sed -i '1{/^#!/d}' exifread/cli.py",
    );
    assert!(unexplained_fixups(ORIGIN, &after, "python-ExifRead").is_empty());
}

#[test]
fn a_comment_run_of_several_lines_counts_when_it_is_directly_above() {
    let after = with_prep_line(
        "# the test data is read through a relative path\n\
         # that the sources do not resolve from the build root\n\
         sed -i 's|data/|tests/data/|' exifread/tests.py",
    );
    assert!(unexplained_fixups(ORIGIN, &after, "python-ExifRead").is_empty());
}

#[test]
fn a_blank_line_between_comment_and_fixup_is_no_comment() {
    let after = with_prep_line(
        "# the script starts with a shebang the wheel does not need\n\n\
         sed -i '1{/^#!/d}' exifread/cli.py",
    );
    assert_eq!(
        unexplained_fixups(ORIGIN, &after, "python-ExifRead").len(),
        1
    );
}

#[test]
fn a_macro_in_the_comment_is_named_as_the_problem() {
    let after = with_prep_line(
        "# the shebang of %{name} cli is not needed here\n\
         sed -i '1{/^#!/d}' exifread/cli.py",
    );
    let problems = unexplained_fixups(ORIGIN, &after, "python-ExifRead");
    assert_eq!(problems.len(), 1, "{problems:?}");
    assert!(problems[0].contains("`%{name}`"), "{problems:?}");
}

#[test]
fn an_escaped_percent_in_the_comment_is_no_macro() {
    let after = with_prep_line(
        "# the evidence line quotes 100%% of the failing run\n\
         sed -i '1{/^#!/d}' exifread/cli.py",
    );
    assert!(unexplained_fixups(ORIGIN, &after, "python-ExifRead").is_empty());
}

#[test]
fn a_comment_too_short_or_only_a_todo_or_the_package_name_is_a_problem() {
    for comment in ["# fix", "# TODO", "# TODO fix", "# python-ExifRead"] {
        let after = with_prep_line(&format!("{comment}\nsed -i 's/a/b/' exifread/cli.py"));
        let problems = unexplained_fixups(ORIGIN, &after, "python-ExifRead");
        assert_eq!(problems.len(), 1, "{comment}: {problems:?}");
        assert!(problems[0].contains("does not say why"), "{problems:?}");
    }
}

#[test]
fn an_inherited_fixup_without_a_comment_is_not_checked() {
    let origin = ORIGIN.replacen(
        "%autosetup -p1 -n ExifRead-%{version}\n",
        "%autosetup -p1 -n ExifRead-%{version}\nsed -i 's/a/b/' exifread/cli.py\n",
        1,
    );
    let after = origin.replacen("%build\n", "%build\n# unrelated new comment here\n", 1);
    assert!(unexplained_fixups(&origin, &after, "python-ExifRead").is_empty());
    // A second copy of the same line is new, so it needs its own comment.
    let twice = origin.replacen(
        "sed -i 's/a/b/' exifread/cli.py\n",
        "sed -i 's/a/b/' exifread/cli.py\nsed -i 's/a/b/' exifread/cli.py\n",
        1,
    );
    assert_eq!(
        unexplained_fixups(&origin, &twice, "python-ExifRead").len(),
        1
    );
}

#[test]
fn a_pytest_skip_line_is_not_a_fixup_of_this_gate() {
    let after = ORIGIN.replacen("%pytest\n", "%pytest -k \"not test_gps\"\n", 1);
    assert!(unexplained_fixups(ORIGIN, &after, "python-ExifRead").is_empty());
}

#[test]
fn every_kind_of_fixup_needs_a_comment() {
    for line in [
        "chmod 0644 exifread/cli.py",
        "mv exifread/cli.py exifread/cli2.py",
        "ln -sf cli.py exifread/cli_link.py",
        "echo 'version = 1' > exifread/_version.py",
        "cp setup.cfg setup.cfg.orig",
        "rm -f exifread/cli.pyc",
        "rm -rf %{buildroot}%{python3_sitelib}/tests",
        "%python_expand rm -rf %{buildroot}%{$python_sitelib}/x",
        "perl -pi -e 's/a/b/' exifread/cli.py",
        "export PYTHONPATH=%{buildroot}%{python3_sitelib}",
        "PYTHONPATH=. %pytest",
    ] {
        let after = with_prep_line(line);
        assert_eq!(
            unexplained_fixups(ORIGIN, &after, "python-ExifRead").len(),
            1,
            "{line}"
        );
    }
}

#[test]
fn a_standard_macro_or_an_absolute_path_removal_is_not_a_fixup() {
    for line in [
        "%pyproject_wheel",
        "rm -rf /var/cache/x",
        "echo done",
        "cp %{SOURCE1} .",
    ] {
        let after = with_prep_line(line);
        assert!(
            unexplained_fixups(ORIGIN, &after, "python-ExifRead").is_empty(),
            "{line}"
        );
    }
}

#[test]
fn a_fixup_outside_the_build_phases_is_not_checked() {
    let after = ORIGIN.replacen("%build\n", "%post\nsed -i 's/a/b/' /etc/x\n%build\n", 1);
    assert!(unexplained_fixups(ORIGIN, &after, "python-ExifRead").is_empty());
}

#[test]
fn a_fixup_in_a_python_expand_block_takes_the_comment_above_the_block() {
    let origin = ORIGIN.replacen(
        "%pyproject_wheel\n",
        "%{python_expand #\n$python -m pip list\n}\n",
        1,
    );
    let edited = origin.replacen(
        "%{python_expand #\n$python -m pip list\n}",
        "%{python_expand #\nsed -i 's/a/b/' exifread/cli.py\n}",
        1,
    );
    assert_eq!(
        unexplained_fixups(&origin, &edited, "python-ExifRead").len(),
        1
    );

    // A comment above the block's first line says why for the whole block.
    let commented = edited.replacen(
        "%{python_expand #\nsed",
        "# the script starts with a shebang the wheel does not need\n%{python_expand #\nsed",
        1,
    );
    assert!(unexplained_fixups(&origin, &commented, "python-ExifRead").is_empty());

    // A comment on a line inside the block does not count: it is not above the block.
    let inside = edited.replacen(
        "sed -i",
        "# the script starts with a shebang the wheel does not need\nsed -i",
        1,
    );
    assert_eq!(
        unexplained_fixups(&origin, &inside, "python-ExifRead").len(),
        1
    );
}

#[test]
fn an_env_assignment_opener_of_a_block_needs_a_comment() {
    let edited = ORIGIN.replacen(
        "%pyproject_wheel\n",
        "%{python_expand PYTHONPATH=%{buildroot}%{$python_sitelib}\n$python -m pytest tests\n}\n",
        1,
    );
    let problems = unexplained_fixups(ORIGIN, &edited, "python-ExifRead");
    assert_eq!(problems.len(), 1, "{problems:?}");
}

#[test]
fn an_empty_origin_gives_no_problem() {
    let after = with_prep_line("sed -i 's/a/b/' exifread/cli.py");
    assert!(unexplained_fixups("", &after, "python-ExifRead").is_empty());
}

/// A path that climbs out of the source tree is not a file the edit changes there,
/// so an `rm` or `cp` of it is no fixup of this gate.
#[test]
fn a_path_that_climbs_out_of_the_source_tree_is_not_a_fixup() {
    for line in ["rm -f ../shared/cache.tar", "cp ../README.rst ."] {
        let after = with_prep_line(line);
        assert!(
            unexplained_fixups(ORIGIN, &after, "python-ExifRead").is_empty(),
            "{line}"
        );
    }
}
