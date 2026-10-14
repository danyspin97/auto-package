# python-nvidia-cuda-nvdisasm notes

## Build requirements

- The upstream source is a prebuilt NVIDIA wheel with no pyproject.toml or setup.cfg. The spec installs the wheel through the pyproject install macro and has no build step, because there is nothing to compile.
- ExclusiveArch follows the architectures the upstream wheel is published for, so the spec is limited to the build architecture's wheel.
- Checked against 13.4.92 on openSUSE_Tumbleweed, x86_64.

## Workarounds

- The generated %check and %files sections contain placeholders for a prebuilt wheel. Attempt 0 filled them by hand from the wheel's RECORD file. The tool should derive them from RECORD itself; the spec should not be hand-edited.
- The one-line description was replaced with the summary text from the wheel's METADATA file. The tool should take it from METADATA.
- Checked against 13.4.92 on openSUSE_Tumbleweed, x86_64.

## Failure history

- The changelog entry could not be written by the submission tool because its session could not reach the keyring; it was written by hand. A later tool change keeps the session bus address so keyring credentials load. Not yet confirmed on this package.
- Checked against 13.4.92 on openSUSE_Tumbleweed, x86_64.
