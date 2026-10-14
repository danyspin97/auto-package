# python-detect-installer

## Build requirements
- BuildRequires uv-build, spelled as its PyPI project name (uv-build, not uv_build).
- The build backend has only a lower bound. Do not cap it below the version the distro ships, or the build cannot resolve it.
Checked against 0.2.1 on openSUSE_Tumbleweed, x86_64.

## Test changes
- The sdist ships no test suite, so %check runs an import smoke test instead.
Checked against 0.2.1 on openSUSE_Tumbleweed, x86_64.

## Workarounds
- The detect-installer-test console script is packaged with python_alternative entries for seven components, so the shared script name is managed through the alternatives mechanism.
Checked against 0.2.1 on openSUSE_Tumbleweed, x86_64.
