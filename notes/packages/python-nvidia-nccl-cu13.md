# python-nvidia-nccl-cu13

**Workarounds**
Checked against 2.29.7 on openSUSE_Tumbleweed, x86_64.
- Source0 is the upstream prebuilt wheel for the build architecture, not a source tarball. %install installs that wheel with the pyproject install macro.
- The wheel is Root-Is-Purelib true, so its files land in sitelib. %files lists the exact paths taken from the wheel's RECORD.
- Any C headers the wheel ships under include/ are not packaged. The spec deletes them, removes their RECORD lines, and drops the devel rpmlint filter that covered them. The reason is not recorded.
- License is BSD-3-Clause AND SUSE-NonFree. The wheel's METADATA declares LicenseRef-NVIDIA-Proprietary, and its License.txt is BSD-3-Clause. This was a pre-build review decision at medium confidence, not confirmed by a build.
