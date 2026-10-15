# python-detect-installer

## Build requirements
- BuildRequires uv-build exactly once, spelled as its PyPI project name (uv-build, not uv_build).
- The build backend requirement has only a lower bound. Do not add an upper bound; see the failure history.
- Requires: none beyond what the packaging tooling generates; upstream declares no runtime dependencies.
Checked against 0.2.1 on openSUSE_Tumbleweed, x86_64.

## Test changes
- The sdist ships no test suite, so %check runs an import smoke test instead.
Checked against 0.2.1 on openSUSE_Tumbleweed, x86_64.

## Workarounds
- The detect-installer-test console script is packaged with python_alternative entries for seven components, so the shared script name is managed through the alternatives mechanism.
Checked against 0.2.1 on openSUSE_Tumbleweed, x86_64.

## Failure history
- OBS rejected the build during dependency resolution because the build backend requirement had an upper bound below 0.10; removing the upper bound fixed it.
Checked against 0.2.1 on openSUSE_Tumbleweed, x86_64.
