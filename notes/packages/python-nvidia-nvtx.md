# python-nvidia-nvtx

## Test changes
- The %check section loads the packaged shared object with dlopen, to confirm the prebuilt library can be loaded.
Checked against 13.0.85 on openSUSE_Tumbleweed, x86_64.

## Workarounds
- The package repackages the prebuilt upstream wheel for the build architecture instead of building from source.
- The wheel is installed into the site-packages directory, not the platform-specific one, because its metadata marks it as purelib-rooted.
- ExclusiveArch follows the platform tag of the upstream wheel, so the package is only built for architectures that wheel targets.
- Header files shipped in the wheel are removed during install, together with their entries in the wheel's RECORD, so they are not packaged.
- The file list is built from the wheel's RECORD rather than by globbing.
- The dist-info license file is kept out of the plain file list and packaged with the license marker instead.
- The License field uses the SPDX identifier of the upstream license.
- Shared-object warnings from rpmlint are covered by the existing rpmlint filter; its development-files filter matches nothing now that headers are removed, which is harmless.
Checked against 13.0.85 on openSUSE_Tumbleweed, x86_64.

## Failure history
- The changelog helper tool fails on a keyring or D-Bus error; fixed by writing the changelog entry by hand.
Checked against 13.0.85 on openSUSE_Tumbleweed, x86_64.
