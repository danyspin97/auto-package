# cuda-cufft

## Test changes
- The spec has no %check section: the prebuilt upstream archive ships no test suite, so there is nothing to run.

Checked against 12.4.0.43 on openSUSE_Tumbleweed, x86_64.

## Workarounds
- rpmlint warnings are suppressed in the package's rpmlintrc for three checks: the FHS 2.3 forbidden-path file list check, the missing hash section check, and the static library without debuginfo check. Each filter has a reason comment in that file.

Checked against 12.4.0.43 on openSUSE_Tumbleweed, x86_64.
