## Build requirements

- The package is the upstream wheel repackaged, so it is installed into sitelib: the wheel is purelib-rooted (Root-Is-Purelib true) even though it contains binaries.
- ExclusiveArch is the architecture of the upstream wheel for the build architecture (x86_64 here). Only that wheel was checked.
- Requires the Python package for nvidia-cuda-nvrtc at runtime, because the wheel's metadata declares it.
Checked against 13.1.1.3 on openSUSE_Tumbleweed, x86_64.

## Workarounds

- The %files list is filled with the exact paths from the wheel's RECORD file, replacing the generator's placeholder. Keep it in sync with RECORD when the version changes.
- The headers bundled in the wheel's include/ directory are removed in %install and left out of %files. The reason is not recorded.
Checked against 13.1.1.3 on openSUSE_Tumbleweed, x86_64.
