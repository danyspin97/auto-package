# python-fastapi-cloud-cli

Notes for 0.26.0. Unlabelled findings are general. Findings from the home:dspinella:open-webui build are kept where this build did not contradict them.

## Build requirements
Checked against 0.26.0 on openSUSE_Tumbleweed, x86_64.
- BuildRequires: pdm-backend, plus every Python package the test suite imports: respx, time_machine, inline_snapshot, click, rich, rich_toolkit, typing_extensions, and the runtime stack.
- Requires: one per main runtime dependency. The generated spec may have none. `email-validator` is required explicitly because the package uses `pydantic[email]`, which only pulls it in as an extra.
- pydantic is bounded per Python version, as the markers in pyproject.toml do.
- typing_extensions is also needed at runtime: the API retry module imports it.
- BuildRequires git-core: the setup-ci test looks for git with `shutil.which`, and the build chroot may not have it.
- BuildRequires the coverage module: test_script runs coverage as a Python module.
- BuildRequires the ruff module: inline-snapshot runs ruff for its format command.
- rich-toolkit has no upper bound on purpose. The pipeline builds the latest PyPI release, and an unsatisfiable bound would make the package unresolvable.
- The package is pure Python: noarch, sitelib, %pytest, no scripts (no [project.scripts]).

## Test changes
Checked against 0.26.0 on openSUSE_Tumbleweed, x86_64.
- 10 tests are deselected in %check with a single-line keyword expression, and a comment cites the version change. rich-toolkit 0.20.6 honours `default=False` and pre-selects No on confirm prompts, while the tests expect the 0.20.5 behaviour.
- With rich-toolkit 0.20.5 the suite passes except for a local-path artifact. With 0.20.6 the same 10 tests fail.
- Remove the deselection once the tests accept the 0.20.6 behaviour, or once rich-toolkit reverts it.

## Workarounds
Checked against 0.26.0 on openSUSE_Tumbleweed, x86_64.
- The upstream metadata names the MIT license but lists no license file, so the LICENSE file is installed into the dist-info licenses directory and marked %license to ship it.

## Failure history
Checked against 0.26.0 on openSUSE_Tumbleweed, x86_64.
- %check: setup-ci reported git as missing. Fixed by BuildRequires git-core.
- %check: test_script failed because the coverage module was missing. Fixed by BuildRequires the coverage module.
- %check: inline-snapshot teardown errored because ruff was missing. Fixed by BuildRequires the ruff module.
- %check: 10 tests failed and 4 errored, all in confirm-prompt tests, with rich-toolkit 0.20.6. Fixed by deselecting them (see Test changes); the build passes with the deselection.
