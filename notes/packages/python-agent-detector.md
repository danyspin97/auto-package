# python-agent-detector

## Build requirements
- The package uses hatchling as its build backend, so hatchling is needed at build time. Upstream declares no runtime dependencies.
- The package is pure Python with no compiled extension, so it is built noarch.
- The package installs a console script, which is registered through the alternatives system, so the spec needs update-alternatives as a Requires for its alternatives scriptlets.
- Checked against 2.0.0 on openSUSE_Tumbleweed, x86_64.

## Test changes
- The project's pytest addopts pull in coverage reporting (--cov), which needs pytest-cov, a plugin the build does not install. The spec clears those addopts in %check so the tests run without coverage. Apply the same rule to any addopts that need a plugin missing from BuildRequires.
- Checked against 2.0.0 on openSUSE_Tumbleweed, x86_64.

## Workarounds
- A console script is wired through the alternatives system, so the spec carries the alternatives components: a clone of the Python package, %post and %postun scriptlets, and a %python_alternative entry in %files. Apply this to any package that installs a console script.
- The changelog entry was written by hand because the changelog tool needs a keyring session, which the build host does not have.
- Checked against 2.0.0 on openSUSE_Tumbleweed, x86_64.
