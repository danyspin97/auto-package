# python-tomli notes

## Build requirements
- BuildRequires the flit_core build backend, within the range the source pyproject.toml declares (flit_core >=3.12, <5), and use the build backend the pyproject names (flit_core.buildapi).
- The spec had a duplicate BuildRequires, which was removed.
- The source has no setup.cfg; build metadata comes from pyproject.toml alone.
Checked against 2.5.0 on openSUSE_Tumbleweed, x86_64.

## Test changes
- %check runs an import smoke test instead of the upstream test suite. The source pyproject.toml declares no test extras.
Checked against 2.5.0 on openSUSE_Tumbleweed, x86_64.
