# python-agent-detector

## Build requirements
- The package uses hatchling as its build backend, so hatchling is needed at build time. It declares no runtime dependencies upstream, so the spec has no runtime Requires.
- The package is pure Python with no compiled extension, so it is built noarch.
- Checked against 2.0.0 on openSUSE_Tumbleweed, x86_64.

## Test changes
- The pytest addopts from the project config pull in coverage reporting (--cov), which needs pytest-cov, a plugin the build does not install. The spec clears those addopts so the tests run without coverage. Apply the same rule to any addopts that need a plugin missing from BuildRequires.
- Checked against 2.0.0 on openSUSE_Tumbleweed, x86_64.

## Workarounds
- The console script needed its alternatives registration added to the spec, so the script is wired through the alternatives system like the other entry points.
- The changelog entry was written by hand because the changelog tool needs a keyring session, which the build environment does not have.
- Checked against 2.0.0 on openSUSE_Tumbleweed, x86_64.
