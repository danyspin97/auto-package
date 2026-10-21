# python-pytest-mock

## Build requirements
- pytest-asyncio is needed for the asyncio tests; the project's test configuration runs asyncio in auto mode.
- setuptools_scm is needed without its toml extra, which upstream no longer provides. The package version is dynamic and comes from setuptools_scm.
- tomli is needed below Python 3.11 to read build metadata.
- mock is in BuildRequires, but the standalone-mock test still skips with "could not import mock" in this build, so that requirement does not satisfy the test.
Checked against 3.17.0 on openSUSE_Tumbleweed, x86_64.

## Test changes
- %check runs pytest with double verbosity (-vv) instead of single (-v). Single verbosity truncates assertion output and hides the inner message for TestIntrospection.test_called_args and test_called_kwargs.
- test_standalone_mock skips when mock cannot be imported. It is a skip, not a failure.
Checked against 3.17.0 on openSUSE_Tumbleweed, x86_64.

## Failure history
- TestIntrospection.test_called_args and test_called_kwargs failed in %check with truncated output. Fixed by running %check with double verbosity; the build then succeeded. The assertion itself was not examined separately.
Checked against 3.17.0 on openSUSE_Tumbleweed, x86_64.
