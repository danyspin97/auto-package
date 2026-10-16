# python-chardet

## Build requirements
- The upstream build backend is hatchling. The build-system table also requires hatch-vcs, which supplies the dynamic version from VCS, so both are BuildRequires.
- The sdist ships src/chardet/_version.py, so the version is read from it rather than from git metadata.
- No runtime dependencies are declared upstream. The console script chardetect comes from the project scripts table.
Checked against 7.4.3 on openSUSE_Tumbleweed, x86_64.

## Test changes
- test_accuracy is ignored. The ignore is passed through PYTEST_ADDOPTS, not as a path argument, because the %pytest macro rejects path arguments. The original reason for ignoring it is not recorded.
- Two tests are deselected: test_none_none_files_have_correct_mime_types and test_all_test_data_pairs_have_models. They get their test data through get_data_dir, which clones chardet/test-data from GitHub, and builds have no network. The other clone tests in test_utils are mocked and need no git.
- Tests import chardet from the source tree on PYTHONPATH, not from the installed package.
Checked against 7.4.3 on openSUSE_Tumbleweed, x86_64.

## Workarounds
- No %prep edits are needed. The sdist has no shebang lines under src/ and no language model files, so the old sed edits from earlier recipes have no targets.
Checked against 7.4.3 on openSUSE_Tumbleweed, x86_64.

## Failure history
- The %check run failed with "Unknown option: /" from pytest, because a path was passed to the %pytest macro. Fixed by moving the test_accuracy ignore into PYTEST_ADDOPTS.
- Two tests failed because they clone test data from GitHub and the build has no network. Fixed by deselecting them.
Checked against 7.4.3 on openSUSE_Tumbleweed, x86_64.
