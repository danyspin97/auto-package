# python-nvidia-cuda-nvrtc

## Build requirements
- The wheel's METADATA declares no Requires-Dist, so the spec needs no Requires beyond what the generator emits.
Checked against 13.0.88 on openSUSE_Tumbleweed, x86_64.

## Workarounds
- The spec's License is SUSE-NonFree. The wheel's METADATA declares LicenseRef-NVIDIA-Proprietary, which the spec does not use.
- Headers that the spec removes from the install are also removed from the RECORD entries, so RECORD lists only files that are installed.
Checked against 13.0.88 on openSUSE_Tumbleweed, x86_64.
