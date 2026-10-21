# python-pytest-xdist notes

## Build requirements
Checked against 3.8.0 on openSUSE_Tumbleweed, x86_64.
- setuptools-scm is a build-system requirement because the project version is dynamic. The spec uses the bare setuptools-scm requirement, not the variant with its extra.
- filelock, psutil and setproctitle are test requirements. They are listed in the project's test extras, so the spec adds them as test BuildRequires.
- pexpect is not in the project's testing extra, so it must be added as a test BuildRequires for the test run.

## Test changes
Checked against 3.8.0 on openSUSE_Tumbleweed, x86_64.
- Two tests that use pytest 9 APIs raise AttributeError when pytest 9 is the test runner, which is the case on this target. They are deselected. One is test_handlecrashitem_one.

## Failure history
Checked against 3.8.0 on openSUSE_Tumbleweed, x86_64.
- The test run needed pexpect, which the testing extra does not provide. Fixed by adding pexpect as a test BuildRequires.
- Two tests failed with AttributeError on pytest 9 APIs. Fixed by deselecting them.
