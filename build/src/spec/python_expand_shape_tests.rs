//! A braced `%{python_expand ...}` block the origin has keeps its shape: an
//! edit to a line inside it passes, an edit that removes, merges, splits or
//! re-emits it is named by the origin's first line.

use super::{braced_python_expand_blocks, reshaped_python_expand_blocks};

/// The python-Cython `%install` as the origin has it: one braced block.
const CYTHON_ORIGIN: &str = "\
%install
%pyproject_install
%python_group_libalternatives cython cythonize cygdb

%{python_expand chmod a+x %{buildroot}%{$python_sitearch}/Cython/Build/Cythonize.py
sed -i \"s|^#!%{_bindir}/env python$|#!%{__$python}|\" %{buildroot}%{$python_sitearch}/Cython/Build/Cythonize.py
$python -m compileall -d %{$python_sitearch} %{buildroot}%{$python_sitearch}/Cython/Build/
$python -O -m compileall -d %{$python_sitearch} %{buildroot}%{$python_sitearch}/Cython/Build/
%fdupes %{buildroot}%{$python_sitearch}
}

%check
";

/// The branch's five-line rewrite of the same block.
const CYTHON_FIVE_LINES: &str = "\
%install
%pyproject_install
%python_group_libalternatives cython cythonize cygdb

%python_expand chmod a+x %{buildroot}%{$python_sitearch}/Cython/Build/Cythonize.py
%python_expand sed -i \"s|^#!%{_bindir}/env python$|#!%{__$python}|\" %{buildroot}%{$python_sitearch}/Cython/Build/Cythonize.py
%python_expand $python -m compileall -d %{$python_sitearch} %{buildroot}%{$python_sitearch}/Cython/Build/
%python_expand $python -O -m compileall -d %{$python_sitearch} %{buildroot}%{$python_sitearch}/Cython/Build/
%python_expand %fdupes %{buildroot}%{$python_sitearch}

%check
";

/// The origin's block with only the sed line changed, inside the block.
const CYTHON_SED_ONLY: &str = "\
%install
%pyproject_install
%python_group_libalternatives cython cythonize cygdb

%{python_expand chmod a+x %{buildroot}%{$python_sitearch}/Cython/Build/Cythonize.py
sed -i \"s|^#!%{_bindir}/env python$|#!%{__$python}|\" %{buildroot}%{$python_sitearch}/Cython/Build/Cythonize.py
sed -i \"1i #!%{__$python}\" %{buildroot}%{$python_sitearch}/Cython/Build/Cythonize.py
$python -m compileall -d %{$python_sitearch} %{buildroot}%{$python_sitearch}/Cython/Build/
$python -O -m compileall -d %{$python_sitearch} %{buildroot}%{$python_sitearch}/Cython/Build/
%fdupes %{buildroot}%{$python_sitearch}
}

%check
";

/// Two origin blocks in one section, the second one `#`-form.
const TWO_BLOCKS: &str = "\
%check
%{python_expand export PYTHONPATH=%{buildroot}%{$python_sitelib}
$python runtests.py -v
}
%{python_expand rm -f conftest.py
}
";

#[test]
fn the_cython_block_is_found_under_install_with_its_first_line() {
    let blocks = braced_python_expand_blocks(CYTHON_ORIGIN);
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0].section, "install");
    assert_eq!(
        blocks[0].first_line,
        "%{python_expand chmod a+x %{buildroot}%{$python_sitearch}/Cython/Build/Cythonize.py"
    );
}

#[test]
fn the_five_line_rewrite_of_the_cython_block_is_a_reshape() {
    let named = reshaped_python_expand_blocks(CYTHON_ORIGIN, CYTHON_FIVE_LINES);
    assert_eq!(
        named,
        vec![
            "%{python_expand chmod a+x %{buildroot}%{$python_sitearch}/Cython/Build/Cythonize.py"
                .to_string()
        ]
    );
}

#[test]
fn a_change_to_one_line_inside_the_block_is_not_a_reshape() {
    assert!(reshaped_python_expand_blocks(CYTHON_ORIGIN, CYTHON_SED_ONLY).is_empty());
}

#[test]
fn a_change_to_the_opener_line_itself_is_not_a_reshape() {
    let edited = CYTHON_ORIGIN.replace("%{python_expand chmod a+x", "%{python_expand chmod 0755");
    assert!(reshaped_python_expand_blocks(CYTHON_ORIGIN, &edited).is_empty());
}

#[test]
fn an_unchanged_spec_is_not_a_reshape() {
    assert!(reshaped_python_expand_blocks(CYTHON_ORIGIN, CYTHON_ORIGIN).is_empty());
}

#[test]
fn a_block_merged_into_one_is_a_reshape() {
    let merged = "\
%check
%{python_expand export PYTHONPATH=%{buildroot}%{$python_sitelib}
$python runtests.py -v
rm -f conftest.py
}
";
    assert_eq!(
        reshaped_python_expand_blocks(TWO_BLOCKS, merged),
        vec!["%{python_expand rm -f conftest.py".to_string()]
    );
}

#[test]
fn a_block_split_in_two_is_a_reshape() {
    let split = "\
%check
%{python_expand export PYTHONPATH=%{buildroot}%{$python_sitelib}
$python runtests.py -v
}
%{python_expand $python -m pytest
}
%{python_expand rm -f conftest.py
}
";
    assert_eq!(
        reshaped_python_expand_blocks(TWO_BLOCKS, split),
        vec!["%{python_expand export PYTHONPATH=%{buildroot}%{$python_sitelib}".to_string()]
    );
}

#[test]
fn a_new_block_in_a_section_with_none_is_not_a_reshape() {
    let origin = "%install\n%pyproject_install\n";
    let added = "%install\n%pyproject_install\n%{python_expand true\n}\n";
    assert!(reshaped_python_expand_blocks(origin, added).is_empty());
}

#[test]
fn a_third_block_in_a_section_with_two_is_a_reshape() {
    let added = TWO_BLOCKS.replace("%check\n", "%check\n%{python_expand rm -f y\n}\n");
    assert_eq!(
        reshaped_python_expand_blocks(TWO_BLOCKS, &added),
        vec!["%{python_expand export PYTHONPATH=%{buildroot}%{$python_sitelib}".to_string()]
    );
}

#[test]
fn a_brace_inside_a_block_does_not_end_it_early() {
    let spec = "\
%install
%{python_expand cp %{buildroot}%{$python_sitelib}/a %{buildroot}%{_bindir}/
$python -c 'print(1)'
}
";
    let blocks = braced_python_expand_blocks(spec);
    assert_eq!(blocks.len(), 1);
    assert!(reshaped_python_expand_blocks(spec, spec).is_empty());
}
