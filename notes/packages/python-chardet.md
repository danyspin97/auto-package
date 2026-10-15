# python-chardet

## Build requirements
- Upstream build backend is hatchling with hatch-vcs; the version is dynamic.
- The sdist ships src/chardet/_version.py, so the version is read from it rather than from git metadata.
- No runtime dependencies are declared upstream. The console script chardetect comes from the project scripts table.
Checked against 7.4.3 on openSUSE_Tumbleweed, x86_64.

## Test changes
- test_accuracy is ignored. The ignore is passed through PYTEST_ADDOPTS, not as a path argument, because the %pytest macro rejects path arguments. The original reason for ignoring it is not recorded.
Checked against 7.4.3 on openSUSE_Tumbleweed, x86_64.

## Workarounds
- No %prep edits are needed. The sdist has no shebang lines under src/ and no language model files, so the old sed edits from earlier recipes have no targets.
Checked against 7.4.3 on openSUSE_Tumbleweed, x86_64.

## Failure history
- The %check run failed with "Unknown option: /" from pytest, because a path was passed to the %pytest macro. Fixed by moving the test_accuracy ignore into PYTEST_ADDOPTS. Checked against 7.4.3 on openSUSE_Tumbleweed, x86_64.
