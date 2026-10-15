# python-omegaconf notes

## Build requirements
Checked against 2.4.0 on openSUSE_Tumbleweed, x86_64.
- The upstream source has no [build-system] table in pyproject.toml; setup.py and setup.cfg drive the build.
- Runtime Requires follow install_requires, which setup.cfg reads from requirements/base.txt. Keep the spec's Requires in step with that file.
- There are no C extensions or entry points, so the build needs no compiler.

## Test changes
Checked against 2.4.0 on openSUSE_Tumbleweed, x86_64.
- %check excludes the CI-workflow, docs-site and website-script tests, because the sdist does not ship those files.
- The upstream pytest configuration sets -Werror, so any warning raised in a test fails that test.

## Failure history
Checked against 2.4.0 on openSUSE_Tumbleweed, x86_64.
- Test collection stopped with 7 FileNotFoundError errors for CI-workflow and website-script paths, before any test ran. Fixed by excluding those tests from %check.
