# python-nvidia-cudnn-cu13 notes

## Build requirements
- The Requires on nvidia-cublas is taken from the upstream wheel's Requires-Dist. Runtime dependencies on other NVIDIA wheel packages should follow the wheel metadata rather than be guessed.
Checked against 9.24.0.43 on openSUSE_Tumbleweed, x86_64.

## Workarounds
- The upstream wheel for the build architecture ships proprietary NVIDIA binaries, so the spec's License is SUSE-NonFree rather than the wheel's LicenseRef-NVIDIA-Proprietary.
- Headers are not installed: they are removed in %install and their entries are removed from RECORD. Reason not recorded.
- %files lists exact paths: the nvidia/cudnn/lib directory and the dist-info directory, with the license file tagged once. No placeholder is left in %files.
Checked against 9.20.0.48 on openSUSE_Tumbleweed, x86_64.
