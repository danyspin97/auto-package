# Catch

## Build requirements
- Catch2 3.x is a CMake project, so CMake is needed at build time. The 1.x header-only recipe needed no build step.
- Catch2 3.x has no USE_CPP11 or USE_CPP14 options; do not pass them.
- The tarball unpacks to Catch2-3.16.1; the setup step must use that directory name.
- Shared libraries are built and go in a runtime subpackage whose name carries the library version. Headers install under include/catch2 and go in the devel package.
Checked against 3.16.1 on openSUSE_Factory, x86_64.

## Failure history
- pkgconfig files (catch2.pc, catch2-with-main.pc) are reported missing when the devel file list names the libdir pkgconfig directory. CMake installs them to the datadir pkgconfig directory, so the devel file list must name that path. Fixed by pointing the devel file list at the datadir path; confirmed by this build.
Checked against 3.16.1 on openSUSE_Factory, x86_64.
