# cpptest notes

## Build requirements
- No library BuildRequires: configure.ac does not use PKG_CHECK_MODULES, so nothing is found through pkg-config.
- Shared library soname: LT_VERSION 1:9:0 produces libcpptest.so.1, so the spec's so_ver must be 1.
Checked against 2.0.1 on openSUSE_Factory, x86_64.

## Test changes
- None. The review found no upstream check target, so the build runs no upstream test suite.
Checked against 2.0.1 on openSUSE_Factory, x86_64.
