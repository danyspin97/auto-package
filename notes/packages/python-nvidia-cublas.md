## Build requirements

- The package is the upstream wheel repackaged, so it is installed into sitelib: the wheel is purelib-rooted (Root-Is-Purelib true) even though it contains binaries.
- ExclusiveArch is the architecture of the upstream wheel for the build architecture. On x86_64 only: the wheel's layout was checked; other architectures are unverified.
- Requires the Python package for nvidia-cuda-nvrtc at runtime, because the wheel's metadata declares it (Requires-Dist).
Checked against 13.1.1.3 on openSUSE_Tumbleweed, x86_64.

## Workarounds

- The %files list is filled with the exact paths from the wheel's RECORD file, replacing the generator's placeholder. Keep it in sync with RECORD when the version changes.
- The headers bundled in the wheel's include/ directory are removed in %install and left out of %files. The reason is not recorded.
- libcuda.so.1 is excluded from auto-requires. It is the driver's library, supplied by the NVIDIA driver rather than a package, so it must not become a dependency.
- The shared libraries bundled in the wheel are excluded from automatic Provides. The reason is not recorded.
Checked against 13.1.1.3 on openSUSE_Tumbleweed, x86_64.
