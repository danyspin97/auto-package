# python-python-docx

## Build requirements

- Runtime Requires on `lxml` and `typing_extensions`: the `docx` package imports both at runtime.
- BuildRequires on `pyparsing`: the test helper imports it.

Checked against 1.2.0 on openSUSE_Tumbleweed, x86_64.

## Test changes

- The test helper uses pyparsing's `DelimitedList` and snake_case names instead of the deprecated camelCase ones. The pyparsing requirement is 3.1 or newer, where those names exist.

Checked against 1.2.0 on openSUSE_Tumbleweed, x86_64.

## Workarounds

- Patch on the test helper: deprecated pyparsing camelCase calls are renamed to snake_case, so the deprecation warnings do not fail collection.

Checked against 1.2.0 on openSUSE_Tumbleweed, x86_64.

## Failure history

- The spec installed the `python_docx/` directory instead of `docx/`; corrected to install `docx/` (caught in the pre-build review).
- Test collection failed on pyparsing's deprecated `delimitedList`: fixed by porting the helper to `DelimitedList` and requiring pyparsing 3.1.
- Test collection still failed on other deprecated camelCase pyparsing calls: fixed by renaming all of them in the helper.

Checked against 1.2.0 on openSUSE_Tumbleweed, x86_64.
