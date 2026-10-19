**Test changes**
- The %check asserts that the installed crt headers exist. There is no import test, because the nvidia directory is a namespace package with no module to import. The build passed with this %check.

Checked against 13.4.92 on openSUSE_Tumbleweed, x86_64.

**Workarounds**
- The package repackages the upstream prebuilt wheel for the build architecture; nothing is compiled. The %files list is derived from the wheel's RECORD: the crt headers, the dist-info directory, and the license file, which is marked %license and excluded from the plain file list.
- The License tag is the wheel metadata's License-Expression, LicenseRef-NVIDIA-Proprietary.
- The crt headers are the whole payload, so they are kept rather than removed by the usual include-deletion step. Removing them would leave an empty package.
- The pre-build review planned to move the headers into a devel subpackage. An older record kept them in the main package with an rpmlintrc filter for devel-file-in-non-devel-package. The journal does not show which layout this build used, and the rpmlint filter's effect is unverified; the build recorded no rpmlint output.

Checked against 13.4.92 on openSUSE_Tumbleweed, x86_64 (the build succeeded; the header layout and rpmlint filter are unconfirmed).
