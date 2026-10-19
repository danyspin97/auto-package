# python-nvidia-cufft notes

## Build requirements
- License: the wheel ships NVIDIA's proprietary license, so the package uses the SUSE-NonFree license group.
- Requires: take the Requires-Dist entries from the upstream wheel METADATA. For 12.0.0.61 this adds nvidia-nvjitlink.
- ExclusiveArch follows the platform tag of the upstream wheel. A manylinux tag means the wheel's architecture, not noarch.
- Install into sitelib when the wheel's WHEEL file says Root-Is-Purelib: true.
- The source is a prebuilt wheel with no build backend (no pyproject.toml or setup.cfg), so no Python build tooling is needed for the build.
Checked against 12.0.0.61 on openSUSE_Tumbleweed, x86_64.

## Workarounds
- %files: list the shared libraries named in the wheel RECORD. Ship the dist-info directory, and put its license file under %license.
- %install: remove the headers the wheel bundles for a CUDA major version other than the package's, so they are not installed.
- Provides/Requires: exclude the bundled shared libraries from automatic dependency generation, since the wheel ships them privately.
- Spec comments: do not put RPM macros in comments; reword them so they do not expand.
Checked against 12.0.0.61 on openSUSE_Tumbleweed, x86_64.

## Failure history
- Changelog entry failed because the changelog tool needs a session message bus that the build environment lacks. Fixed by writing the .changes entry by hand.
Checked against 12.0.0.61 on openSUSE_Tumbleweed, x86_64.
