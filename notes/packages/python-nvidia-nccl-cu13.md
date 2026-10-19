# python-nvidia-nccl-cu13

**Workarounds**
Checked against 2.30.7 on openSUSE_Tumbleweed, x86_64.
- Source0 is the upstream prebuilt wheel for the build architecture, not a source tarball. %install installs that wheel with the pyproject install macro.
- The wheel is Root-Is-Purelib true, so its files land in sitelib. %files lists the exact paths taken from the wheel's RECORD.
- The wheel bundles its own libnccl.so.2, which is excluded from automatic provides and requires. This was set in the pre-build review and not re-checked against the spec for this note.
- Any C headers the wheel ships under include/ are not packaged. The spec deletes them, removes their RECORD lines, and drops the devel rpmlint filter that covered them. The reason is not recorded.
- License is BSD-3-Clause AND SUSE-NonFree. The wheel's METADATA declares LicenseRef-NVIDIA-Proprietary, and its License.txt is BSD-3-Clause. A successful build does not confirm the license choice; it remains a medium-confidence review decision.
