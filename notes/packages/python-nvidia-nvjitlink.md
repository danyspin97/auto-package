# python-nvidia-nvjitlink notes

## Build requirements
- The package repackages the upstream wheel for the build architecture rather than compiling, so no compiler is needed.
- An automatically generated requirement on libcuda.so.1 is filtered out. The CUDA driver library comes from the NVIDIA driver, not from a package this one should pull in.
Checked against 13.4.92 on openSUSE_Tumbleweed, x86_64.

## Workarounds
- Headers bundled in the upstream wheel are removed at install time, so this package does not install them.
- The license file is excluded from the dist-info glob and tagged as %license, so it is not installed as documentation.
- %files lists the exact paths from the wheel's RECORD file instead of a wildcard.
- The changelog entry was written by hand because the packaging tool could not reach the credential keyring from the build session. This is a tool-side problem; the entry should come from the tool, not from the spec.
Checked against 13.4.92 on openSUSE_Tumbleweed, x86_64.
