**Build requirements**
- The C++ library is compiled during the build, so the build needs cmake and a C++ compiler (gcc-c++).
- The pytest package is not needed: the tests are a doctest script, not a pytest suite.
Checked against 4.15.4.0 on openSUSE_Tumbleweed, x86_64.

**Test changes**
- The check section runs the upstream doctest script directly, not the pytest macro.
Checked against 4.15.4.0 on openSUSE_Tumbleweed, x86_64.

**Workarounds**
- Install the Python package into the sitelib directory, not sitearch. A pure-Python upstream wheel (py3-none tag) is installed into purelib, so every path that refers to the package (removal of the include directory, duplicate-file removal, file list, test PYTHONPATH) must use sitelib.
- The package stays architecture-specific because it ships a compiled libz3 shared library.
- The changelog entry is written by the tooling; do not hand-write it.
Checked against 4.15.4.0 on openSUSE_Tumbleweed, x86_64.

**Failure history**
- Test failure, z3 not importable: the package was installed to sitelib but the test PYTHONPATH pointed at sitearch. Fixed by using sitelib everywhere (see Workarounds).
- Before the first build, the spec had the wrong import directory (z3_solver instead of z3), an invalid summary and license, and missing cmake and gcc-c++; all were corrected.
Checked against 4.15.4.0 on openSUSE_Tumbleweed, x86_64.
