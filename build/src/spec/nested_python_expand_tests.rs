//! A `%python_expand ` line nested in a braced `%{python_expand ...}` block is
//! found, and an origin's own nested line is inherited rather than fresh.

use super::{fresh_nested_python_expand_lines, nested_python_expand_lines};

/// The python-Beaker `%check` as the origin has it: a valid braced opener with
/// an environment assignment as its first token.
const BEAKER_ORIGIN: &str = "\
%check
%{python_expand PYTHONPATH=%{buildroot}%{$python_sitelib}
pytest tests
}
";

/// The same block as the agent left it: the opener became the `#` form and the
/// `export` line carries a `%python_expand ` prefix inside the block.
const BEAKER_EDITED: &str = "\
%check
%{python_expand #
%python_expand export PYTHONPATH=%{buildroot}%{$python_sitelib}
$python -m pytest tests
}
";

/// The same block with the origin's opener and only the body changed to the
/// `$python` form, as convention 19 (section 119) asks.
const BEAKER_FIXED: &str = "\
%check
%{python_expand PYTHONPATH=%{buildroot}%{$python_sitelib}
$python -m pytest tests
}
";

#[test]
fn the_beaker_nested_line_is_found_with_its_number() {
    assert_eq!(
        nested_python_expand_lines(BEAKER_EDITED),
        vec![(
            3,
            "%python_expand export PYTHONPATH=%{buildroot}%{$python_sitelib}".to_string()
        )]
    );
}

#[test]
fn the_origin_opener_and_a_plain_body_are_clean() {
    assert!(nested_python_expand_lines(BEAKER_ORIGIN).is_empty());
    assert!(nested_python_expand_lines(BEAKER_FIXED).is_empty());
}

#[test]
fn a_hash_opener_with_a_plain_body_is_clean() {
    let spec = "%{python_expand #\nexport PYTHONPATH=%{buildroot}%{$python_sitelib}\n$python -m pytest\n}\n";
    assert!(nested_python_expand_lines(spec).is_empty());
}

#[test]
fn an_indented_nested_line_is_found() {
    let spec = "%check\n%{python_expand #\n  %python_expand test -e a\n}\n";
    assert_eq!(
        nested_python_expand_lines(spec),
        vec![(3, "  %python_expand test -e a".to_string())]
    );
}

#[test]
fn a_commented_line_inside_a_block_is_not_found() {
    let spec = "%{python_expand #\n# %python_expand test -e a\n$python -m pytest\n}\n";
    assert!(nested_python_expand_lines(spec).is_empty());
}

#[test]
fn a_one_line_block_does_not_open_a_nested_region() {
    let spec = "%install\n%{python_expand rm -f a}\n%python_expand chmod +x a\n";
    assert!(nested_python_expand_lines(spec).is_empty());
}

#[test]
fn a_bare_python_expand_line_after_a_closed_block_is_not_found() {
    let spec = format!("{BEAKER_FIXED}%python_expand chmod +x a\n");
    assert!(nested_python_expand_lines(&spec).is_empty());
}

#[test]
fn a_macro_in_the_opener_does_not_end_the_block_early() {
    let spec =
        "%install\n%{python_expand rm %{buildroot}%{$python_sitearch}/a\n%python_expand rm b\n}\n";
    assert_eq!(
        nested_python_expand_lines(spec),
        vec![(3, "%python_expand rm b".to_string())]
    );
}

#[test]
fn an_edit_that_adds_the_nested_prefix_is_fresh() {
    let fresh = fresh_nested_python_expand_lines(BEAKER_ORIGIN, BEAKER_EDITED);
    assert_eq!(fresh.len(), 1);
    assert_eq!(fresh[0].0, 3);
}

#[test]
fn a_nested_line_the_origin_already_has_is_inherited() {
    assert!(fresh_nested_python_expand_lines(BEAKER_EDITED, BEAKER_EDITED).is_empty());
    let origin_has_it = format!("{BEAKER_EDITED}\n");
    let edited = format!("{origin_has_it}%check\n");
    assert!(fresh_nested_python_expand_lines(&origin_has_it, &edited).is_empty());
}

#[test]
fn fixing_the_body_is_not_a_fresh_nested_line() {
    assert!(fresh_nested_python_expand_lines(BEAKER_ORIGIN, BEAKER_FIXED).is_empty());
}
