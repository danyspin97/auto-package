# python-nvidia-nvtx

## Workarounds
- The upstream wheel for the build architecture is installed into the site-packages directory, because its metadata marks it as purelib-rooted. The package is restricted to the architecture the wheel targets.
- Header files shipped in the wheel are removed during install, together with their entries in the wheel's RECORD, so they are not packaged.
- The dist-info license file is kept out of the plain file list and packaged with the license marker instead.
- The file list is built from the wheel's RECORD rather than by globbing.
- The License field uses the SPDX identifier of the upstream license.
- Shared-object warnings from rpmlint are covered by the package's existing rpmlint filter. Its filter for development files matches nothing now that headers are removed, which is harmless.
Checked against 13.0.85 on openSUSE_Tumbleweed, x86_64 (pre-build review; the build succeeded).
