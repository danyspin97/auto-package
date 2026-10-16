# cuda-cusparse notes

## Test changes
- No %check section. The archive holds prebuilt NVIDIA binaries with no test suite, so a comment stands where %check would go. rpmlint reports the missing section as a warning only.

Checked against 12.8.6.72 on openSUSE_Tumbleweed, x86_64.

## Workarounds
- Strip the $ORIGIN RUNPATH from the shipped libcusparse shared libraries. rpmlint reports binary-or-shlib-defines-rpath as an error otherwise.
- Ship the Fortran wrapper source file in the devel package, not the main package. rpmlint reports devel-file-in-non-devel-package as an error otherwise.

Checked against 12.8.6.72 on openSUSE_Tumbleweed, x86_64.

## Failure history
- Build failed in rpmlint, not in %check, on the two errors above. Fixed by stripping the RUNPATH and moving the Fortran source to devel.

Checked against 12.8.6.72 on openSUSE_Tumbleweed, x86_64.
