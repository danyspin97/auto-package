# python-nvidia-cufft notes

## Build requirements
- Requires: take the Requires-Dist entries from the wheel METADATA. For this package that adds nvidia-nvjitlink.
- ExclusiveArch comes from the platform tag of the wheel. A manylinux tag means ExclusiveArch for that architecture, not noarch. On x86_64 only: ExclusiveArch x86_64.
- Install into sitelib when the wheel's WHEEL file says Root-Is-Purelib: true.
Checked against 12.0.0.61 on openSUSE_Tumbleweed, x86_64.

## Workarounds
- %files: list the shared libraries named in the wheel RECORD. Ship the dist-info directory, and put its license file under %license.
- %install: remove the headers the wheel bundles for CUDA major version 13, so they are not installed into this CUDA 12 package.
- Spec comments: do not put RPM macros in comments; reword them so they do not expand.
Checked against 12.0.0.61 on openSUSE_Tumbleweed, x86_64.

## Failure history
- Changelog entry failed because the changelog tool needs a session message bus that the build environment lacks. Fixed by writing the .changes entry by hand.
Checked against 12.0.0.61 on openSUSE_Tumbleweed, x86_64.
