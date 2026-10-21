# python-build notes

## Build requirements
- Test BuildRequires: pytest-mock, filelock, setuptools, pytest-rerunfailures, virtualenv. The tests import pytest-mock and filelock and create virtualenvs. test_main.py uses the flaky marker, which only pytest-rerunfailures registers; without it collection fails.
- Runtime Requires: tomli and importlib-metadata are conditional on the Python version, needed only where the standard library lacks them.
- pyproject-hooks is referenced by its hyphenated RPM name.
Checked against 1.6.1 on openSUSE_Tumbleweed, x86_64.

## Test changes
- Tests that run pip inside isolated environments are deselected. The build environment has no network and no CA bundle, so pip cannot install anything there.
Checked against 1.6.1 on openSUSE_Tumbleweed, x86_64.

## Workarounds
- The pyproject wheel step writes its output to a directory outside the source tree. The default output directory is ./build, and the project has its own build module, so the flit build backend otherwise finds two modules named build.
Checked against 1.6.1 on openSUSE_Tumbleweed, x86_64.

## Failure history
- Flit build backend found two modules named build (wheel output inside the source tree): wheel output moved outside the source tree.
- Test collection failed on the unknown flaky marker: added pytest-rerunfailures as a test BuildRequires.
- Tests failed with no module named virtualenv: added virtualenv as a test BuildRequires.
- Isolated-environment pip tests failed for lack of a CA bundle and network: deselected those tests.
Checked against 1.6.1 on openSUSE_Tumbleweed, x86_64.
