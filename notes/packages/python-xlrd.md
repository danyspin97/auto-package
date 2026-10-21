# python-xlrd

## Test changes
- The upstream release ships no test suite, so `%check` runs an import smoke test of the module instead of a test run.
- Checked against 2.0.2 on openSUSE_Tumbleweed, x86_64. This comes from the pre-build review of the spec; the build reported success, but the spec was not re-read after it.
