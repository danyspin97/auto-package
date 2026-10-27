# cmark notes

## Build requirements
- Upstream 0.31.2 sets its CMake minimum to 3.14, so the spec must require at least that version. A CMake older than 3.14 cannot configure the project.
- The library subpackage name carries the SONAME version (libcmark0_31_2 for 0.31.2), and the files section lists the versioned shared object. Each version bump renames the subpackage.
Checked against 0.31.2 on openSUSE_Factory, x86_64.

## Test changes
- Upstream 0.31.2 removed the CMARK_TESTS option. Tests are now controlled by the standard BUILD_TESTING option, so the spec sets BUILD_TESTING to OFF at configure time and the test suite is neither built nor run.
Checked against 0.31.2 on openSUSE_Factory, x86_64.

## Workarounds
- Upstream 0.31.2 removed the CMARK_STATIC option. Passing it only prints a deprecation warning and maps it to the shared-library switch, so the spec no longer passes it.
Checked against 0.31.2 on openSUSE_Factory, x86_64.
