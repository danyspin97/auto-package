# python-python-docx

## Build requirements

- Runtime Requires on `lxml` and `typing_extensions`: the `docx` package imports both at runtime.

Checked against 1.2.0 on openSUSE_Tumbleweed, x86_64.

## Test changes

- The test helper uses pyparsing's `DelimitedList` and snake_case names (including `remove_quotes`) instead of the deprecated camelCase ones. These names need pyparsing 3.1 or newer. The spec does not declare pyparsing in BuildRequires as of this build.

Checked against 1.2.0 on openSUSE_Tumbleweed, x86_64.

## Workarounds

- Patch on the test helper (`0001-Use-snake-case-pyparsing-names-in-test-helper.patch`): deprecated pyparsing camelCase calls are renamed to snake_case, so the deprecation warnings do not fail collection or tests.
- Patches are created and refreshed only with quilt in the source tree. A hand-written patch has wrong hunk headers and does not apply.

Checked against 1.2.0 on openSUSE_Tumbleweed, x86_64.

## Failure history

- The spec installed the `python_docx/` directory instead of `docx/`: fixed to install `docx/` (caught in the pre-build review).
- Test collection failed on pyparsing's deprecated `delimitedList`: fixed by porting the helper to `DelimitedList` and requiring pyparsing 3.1.
- Test collection still failed on other deprecated camelCase pyparsing calls: fixed by renaming all of them in the helper.
- Pytest reported 65 failures and 4 errors from the deprecated `removeQuotes` warning: fixed by renaming it to `remove_quotes` and refreshing the patch with quilt.
- A hand-written helper patch did not apply ("No file to patch"): fixed by regenerating it with quilt.

Checked against 1.2.0 on openSUSE_Tumbleweed, x86_64.
