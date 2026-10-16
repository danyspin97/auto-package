# cuda-cublas notes

## Build requirements
- Source0 is the upstream prebuilt archive for the build architecture; the spec is ExclusiveArch x86_64 and unpacks the archive directory named for the version.
- Checked against 13.8.0.4 on openSUSE_Tumbleweed, x86_64.

## Test changes
- The prebuilt archive has no test suite, so nothing is run in %check.
- Checked against 13.8.0.4 on openSUSE_Tumbleweed, x86_64.

## Workarounds
- Fortran .c sources under the CUDA src directory belong in the -devel subpackage, not the main package.
- The $ORIGIN RUNPATH is removed from the shipped shared objects to get below the rpmlint badness threshold.
- Checked against 13.8.0.4 on openSUSE_Tumbleweed, x86_64.

## Failure history
- rpmlint badness 50100 over the 999 threshold (devel-file-in-non-devel-package for the Fortran .c sources): fixed by moving those sources to -devel and removing the RUNPATH, after which the build passed. Checked against 13.8.0.4 on openSUSE_Tumbleweed, x86_64.
