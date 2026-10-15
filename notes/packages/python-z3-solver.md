**Build requirements**
- The C++ library is compiled during the build, so the build needs cmake and a C++ compiler (gcc-c++).
- The pytest package is not needed: the tests are a doctest script, not a pytest suite.
Checked against 4.15.4.0 on openSUSE_Tumbleweed, x86_64.

**Test changes**
- The check section runs the upstream doctest script directly, not the pytest macro.
Checked against 4.15.4.0 on openSUSE_Tumbleweed, x86_64.

**Workarounds**
- Install the Python package into the sitelib directory, not sitearch. The upstream wheel for the build architecture is installed into purelib, so every path that refers to the package (duplicate-file removal, file list, test PYTHONPATH) must use sitelib.
- The package stays architecture-specific because it ships a compiled libz3 shared library.
- Keep the z3 C headers (the z3/include directory) in the installed package: the upstream wheel ships them and the build does not delete them. Put them in a -devel subpackage, because rpmlint flags headers in a non-devel package.
- Filter the ldconfig errors that the bundled libz3 library produces during the install step.
- The changelog entry is written by the tooling; do not hand-write it.
Checked against 4.15.4.0 on openSUSE_Tumbleweed, x86_64.

**Failure history**
- Test failure, z3 not importable: the package was installed to sitelib but the test PYTHONPATH pointed at sitearch. Fixed by using sitelib everywhere.
- rpmlint aborted the build (badness 2704 over the threshold of 999): 54 z3 header files in non-devel Python subpackages, 50 each. Fixed by moving the headers to a -devel subpackage.
- Before the first build: wrong import directory (z3_solver instead of z3), invalid summary and license, and missing cmake and gcc-c++. All corrected.
Checked against 4.15.4.0 on openSUSE_Tumbleweed, x86_64.
