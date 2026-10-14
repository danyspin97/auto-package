**Test changes**
- The %check asserts that the installed crt headers exist. There is no import test, because the nvidia directory is a namespace package with no module to import. The build passed with this %check.

Checked against 13.4.92 on openSUSE_Tumbleweed, x86_64.

**Workarounds**
- Source: the package repackages the upstream prebuilt wheel for the build architecture rather than compiling anything. The %files list is derived from the wheel's RECORD: the crt headers, the dist-info directory, and the license file, which is marked %license and excluded from the plain file list.
- The crt headers stay in the package and are not removed by the usual include-deletion step. They are the whole payload, so removing them would leave an empty package.
- An rpmlintrc source file filters devel-file-in-non-devel-package for the crt headers, because the headers ship in a non-devel package by design. The build did not record rpmlint output, so whether the filter takes effect is unconfirmed.

Checked against 13.4.92 on openSUSE_Tumbleweed, x86_64 (the build succeeded; the rpmlint filter is unverified).
