# python-starsessions notes

## Build requirements
- The build backend is poetry-core, declared in pyproject.toml.
- starlette and itsdangerous are required (non-optional) runtime dependencies. Add them as BuildRequires as well, because the %check imports the package, which loads them.
- redis is an optional extra: Recommends it, do not Require it.
- Runtime Requires should match the non-optional entries of the pyproject dependency table.
Checked against 2.2.1 on openSUSE_Tumbleweed, x86_64.

## Test changes
- The sdist has no tests directory, although pyproject declares a pytest config. The %check is therefore an import smoke test of the starsessions package, not the upstream test suite. The build passed with it.
Checked against 2.2.1 on openSUSE_Tumbleweed, x86_64.
