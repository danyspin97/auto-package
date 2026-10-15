# python-nvidia-cuda-nvdisasm notes

## Build requirements

- The upstream source is a prebuilt NVIDIA wheel for the build architecture, with no pyproject.toml, setup.cfg or setup.py. The spec installs it through the pyproject install macro with no build step, because there is nothing to compile.
- ExclusiveArch follows the architectures the upstream wheel is published for, so the spec is limited to the build architecture's wheel.
- Checked against 13.4.92 on openSUSE_Tumbleweed, x86_64.

## Test changes

- The wheel ships no test suite, only the nvdisasm binary and dist-info files. %check runs a version smoke test of the bundled binary instead.
- Checked against 13.4.92 on openSUSE_Tumbleweed, x86_64.

## Workarounds

- The generated %check and %files sections are placeholders for a prebuilt wheel. The tool must derive them from the wheel's RECORD file; hand-editing the spec is not an acceptable fix.
- The one-line description is taken from the wheel's METADATA summary.
- The license is SUSE-NonFree, chosen for the wheel's LicenseRef-NVIDIA-Proprietary expression.
- Checked against 13.4.92 on openSUSE_Tumbleweed, x86_64.

## Failure history

- The changelog entry failed to submit because the submission session could not reach the keyring. A later tool change keeps the session bus address so keyring credentials load; not yet confirmed on this package.
- Checked against 13.4.92 on openSUSE_Tumbleweed, x86_64.
