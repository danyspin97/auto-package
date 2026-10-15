# python-langchain-text-splitters

## Build requirements
- Requires langchain-core at runtime: the splitters build on its document and base classes, so the package cannot be imported without it.
- Test dependencies are added as BuildRequires so the unit tests can run during the build.
Checked against 1.1.2 on openSUSE_Tumbleweed, x86_64.

## Test changes
- Only the unit tests are run; the integration tests are not selected.
Checked against 1.1.2 on openSUSE_Tumbleweed, x86_64.
