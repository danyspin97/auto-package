# python-nvidia-cusparse

## Build requirements
- The spec repackages the upstream wheel for the build architecture; it does not compile from source.
- Requires nvidia-nvjitlink, because the bundled libraries need nvJitLink at runtime.
- ExclusiveArch matches the architecture of the upstream wheel, since the package only repackages it. Files go to sitelib because the wheel declares Root-Is-Purelib: true.
Checked against 12.6.3.3 on openSUSE_Tumbleweed, x86_64.

## Workarounds
- The CUDA headers bundled in the wheel are removed in %install and dropped from the file list, so the package does not ship them.
- The automatic requirement on libcuda.so.1 is excluded, so the package does not require the driver-provided library.
- The %files list is written out exactly from the wheel's RECORD file.
Checked against 12.6.3.3 on openSUSE_Tumbleweed, x86_64.
