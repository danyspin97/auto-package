# python-nvidia-cuda-cupti notes

## Build requirements
Checked against 13.0.85 on openSUSE_Tumbleweed, x86_64.
- The package repackages the prebuilt manylinux wheel that upstream publishes for the build architecture. It ships shared libraries, but there is no source build, so no compiler or build-time library is needed.

## Workarounds
Checked against 13.0.85 on openSUSE_Tumbleweed, x86_64.
- The upstream wheel's CUDA include/ tree is stripped from the installed package, and its RECORD entries are dropped too. This follows packaging convention 24, which keeps bundled headers out of the package.
- The spec tags the licence as SUSE-NonFree rather than the upstream LicenseRef-NVIDIA-Proprietary, and the Summary has no trailing period, which the upstream metadata has.
- License.txt is excluded from the dist-info payload and installed as a license file instead, so it is packaged under the license macro.
- %files lists the files explicitly: namespace directories are owned with %dir and every library is named, so no file is left unowned.
- The changelog entry was written by hand. The changelog tool fails without a D-Bus session, which the build agent does not have.
