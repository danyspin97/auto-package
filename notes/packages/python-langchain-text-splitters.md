# python-langchain-text-splitters

## Build requirements
- Requires langchain-core at runtime: the splitters build on its document and base classes, so the package cannot be imported without it. Keep the bound within the upstream range, at least 1.2.31 and below 2.0.0.
- Test dependencies are added as BuildRequires so the unit tests can run during the build.
Checked against 1.1.2 on openSUSE_Tumbleweed, x86_64.

## Test changes
- Only the unit tests are run; the integration tests are not selected.
- %check runs pytest on the unit test directory, not the tests root. The unit conftest defines the custom option the run needs, and pytest only loads it before collection when it starts there.
Checked against 1.1.2 on openSUSE_Tumbleweed, x86_64.

## Failure history
- Running pytest on the tests root failed with an INTERNALERROR about an unknown option; fixed by pointing %check at the unit test directory.
Checked against 1.1.2 on openSUSE_Tumbleweed, x86_64.
