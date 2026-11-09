//! `fix_stray_dollar_macros` must leave a `%{$python_...}` reference alone when
//! a `%{python_expand ...}` block encloses it, even when the block spans lines.

use super::fix_stray_dollar_macros;

/// The python-aioquic 1.3.0 block: its opener, a `%{$...}` reference and its
/// closing `}` sit on three lines.
const AIOQUIC_BLOCK: &str = "\
%install
%{python_expand rm %{buildroot}%{$python_sitearch}/aioquic/*.c
%fdupes %{buildroot}%{$python_sitearch}
}
";

#[test]
fn a_multi_line_python_expand_block_is_left_alone() {
    assert_eq!(fix_stray_dollar_macros(AIOQUIC_BLOCK), AIOQUIC_BLOCK);
}

#[test]
fn a_one_line_python_expand_block_is_left_alone() {
    let spec = "%install\n%{python_expand rm %{buildroot}%{$python_sitearch}/x}\n";
    assert_eq!(fix_stray_dollar_macros(spec), spec);
}

#[test]
fn a_bare_line_after_a_closed_block_is_still_fixed() {
    let spec = format!("{AIOQUIC_BLOCK}chmod +x %{{$python_sitelib}}/a\n");
    let fixed = fix_stray_dollar_macros(&spec);
    assert!(
        fixed.starts_with(AIOQUIC_BLOCK),
        "the block changed: {fixed}"
    );
    assert!(
        fixed.ends_with("%python_expand chmod +x %{$python_sitelib}/a\n"),
        "{fixed}"
    );
}

#[test]
fn a_bare_line_outside_any_block_is_fixed() {
    let spec = "%install\nchmod +x %{$python_sitelib}/a\n";
    assert_eq!(
        fix_stray_dollar_macros(spec),
        "%install\n%python_expand chmod +x %{$python_sitelib}/a\n"
    );
}

#[test]
fn an_unbalanced_block_is_left_alone() {
    let spec = "\
%install
%{python_expand rm %{buildroot}%{$python_sitearch}/aioquic/*.c
chmod +x %{$python_sitelib}/b
%fdupes %{buildroot}%{$python_sitearch}
";
    assert_eq!(fix_stray_dollar_macros(spec), spec);
}
