# python-nvidia-cuda-runtime notes

**Build requirements**
- The upstream metadata declares no Requires-Dist and no entry points, so no runtime Requires are derived from the wheel.
- The upstream wheel for the build architecture is marked Root-Is-Purelib with manylinux platform tags.
- The upstream metadata declares a proprietary NVIDIA license expression; the spec uses the SUSE-NonFree license tag instead.
Checked against 13.0.96 on openSUSE_Tumbleweed, x86_64 (pre-build review of the sdist; the build then succeeded).

**Workarounds**
- The spec drops the static archives and headers from the installed files. The reason was not recorded in the journal.
Checked against 13.0.96 on openSUSE_Tumbleweed, x86_64 (pre-build review; the build then succeeded).
