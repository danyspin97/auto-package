# python-nvidia-cudnn-cu13 notes

## Build requirements
- The Requires on nvidia-cublas is taken from the upstream wheel's Requires-Dist. Runtime dependencies on other NVIDIA wheel packages should follow the wheel metadata rather than be guessed.
Checked against 9.20.0.48 on openSUSE_Tumbleweed, x86_64.

## Workarounds
- The package ships a prebuilt upstream wheel, so the spec sets License to LicenseRef-NVIDIA-Proprietary.
- %files lists exact paths: the nvidia/cudnn/lib directory and the dist-info directory, with the license file tagged once. No placeholder is left in %files.
- Headers are not installed; they are removed in %install. The reason is not recorded in the journal, so check it before relying on it.
Checked against 9.20.0.48 on openSUSE_Tumbleweed, x86_64.

## Failure history
- None recorded for this package. The only journal entry is a pre-build review of the spec.
Checked against 9.20.0.48 on openSUSE_Tumbleweed, x86_64.
