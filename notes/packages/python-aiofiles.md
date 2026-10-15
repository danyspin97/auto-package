# python-aiofiles

## Build requirements

- Build backend is hatchling, with hatch-vcs for the version. The upstream pyproject.toml declares both, so the spec BuildRequires them. poetry-core is no longer needed and was dropped.
- No runtime Requires beyond the defaults: upstream declares no dependencies.

Checked against 25.1.0 on openSUSE_Tumbleweed, x86_64.

## Workarounds

- Planned, not confirmed by any recorded failure: set the setuptools-scm pretend-version environment variable to the package version, so version detection works when the source has no git metadata. Keep only if a later build shows it is needed.

Checked against 25.1.0 on openSUSE_Tumbleweed, x86_64.
