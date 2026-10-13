# python-fastapi-cloud-cli

Notes from the 0.26.0 build in home:dspinella:open-webui / openSUSE_Tumbleweed. Unlabelled findings are general.

## Build requirements
- BuildRequires: pdm-backend, plus every Python package imported by %check: respx, time_machine, inline_snapshot, click, rich, rich_toolkit, typing_extensions, and the runtime stack.
- Requires: one per main runtime dependency. `email-validator` is required explicitly because the package uses `pydantic[email]`, which only pulls it in as an extra.
- BuildRequires `git-core` (home:dspinella:open-webui / openSUSE_Tumbleweed): the setup-ci test looks for git with `shutil.which`, and the chroot has none.
- BuildRequires `%{python_module coverage}`: test_script runs `python -m coverage`.
- BuildRequires `%{python_module ruff}`: inline-snapshot shells out to ruff for its format-command.
- rich-toolkit has no upper bound on purpose. The pipeline builds the latest PyPI release, and an unsatisfiable bound would make the package unresolvable.
- The package is noarch with sitelib and `%pytest`. It has no C extensions, scripts, or CUDA dependencies.

## Test changes
- 10 tests are deselected with a single-line `-k` expression in %check, with a comment citing the version change. rich-toolkit 0.20.6 honours `default=False` and pre-selects No on confirm prompts, while the tests expect 0.20.5 behaviour.
- Verified locally: with rich-toolkit 0.20.5 the suite passes except for a local-path artifact. With 0.20.6 the same 10 tests fail.
- Remove the deselection once the tests accept the 0.20.6 behaviour, or once rich-toolkit reverts it.

## Failure history
- %check: setup-ci reported "git is not installed" (home:dspinella:open-webui / openSUSE_Tumbleweed). Fixed by BuildRequires `git-core`.
- %check: test_script failed because `python -m coverage` was missing. Fixed by BuildRequires `%{python_module coverage}`.
- %check: inline-snapshot teardown ERROR because ruff was missing. Fixed by BuildRequires `%{python_module ruff}`.
- %check: 10 confirm-prompt tests failed with rich-toolkit 0.20.6. Fixed by deselecting them (see Test changes).
