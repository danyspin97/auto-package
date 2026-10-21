# python-pytest-asyncio

## Build requirements

- BuildRequires setuptools_scm as a plain package instead of the bracketed setuptools-scm[toml] form.
- BuildRequires hypothesis, because the test suite needs it.
- Requires pytest with an upper bound. The journal does not record the reason.
- Requires backports and typing_extensions only on the Python versions that need them (version-gated).

Checked against 1.4.0 on openSUSE_Tumbleweed, x86_64.

## Test changes

- Six pytester tests are deselected. Their exact warning-count assertions are off by one under Python 3.14 (one expects 4 warnings and gets 5, another expects 0 and gets 1).
- The journal records 16 failing pytester tests, all asserting warning counts, but only six are deselected. Why the other ten pass is not recorded.

Checked against 1.4.0 on openSUSE_Tumbleweed, x86_64.

## Workarounds

- SETUPTOOLS_SCM_PRETEND_VERSION is exported during the build so the version is set without VCS metadata (inferred reason).
- No patches or source fetches are recorded.

Checked against 1.4.0 on openSUSE_Tumbleweed, x86_64.

## Failure history

- Pytester warning-count tests failed under Python 3.14: six were deselected (see Test changes). The build then succeeded; the other ten are not explained.

Checked against 1.4.0 on openSUSE_Tumbleweed, x86_64.
