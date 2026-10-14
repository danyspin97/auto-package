# python-nvidia-cuda-nvcc

Prebuilt CUDA nvcc wheel, packaged from its unpacked layout. Last built: 13.4.92.

## Build requirements
Checked against 13.4.92 on openSUSE_Tumbleweed, x86_64.
- Import name is `nvidia`, a namespace package installed to sitelib. Its `nvidia/` directory is shared with other nvidia-* packages, so %files lists it with %dir.
- Requires and BuildRequires: `nvidia-nvvm`, `nvidia-cuda-runtime`, `nvidia-cuda-crt`, the wheel's Requires-Dist. BuildRequires mirror them so %check can import the package and run nvcc (inferred; the reason is not recorded).

## Test changes
Checked against 13.4.92 on openSUSE_Tumbleweed, x86_64.
- %check imports the `cu13` namespace of the package and checks that nvcc is executable, once per flavor.

## Workarounds
Checked against 13.4.92 on openSUSE_Tumbleweed, x86_64.
- Headers are removed from the package, and their RECORD lines are dropped from the dist-info so the file list matches. The reason is not recorded.
- The license is owned once through %license and is not duplicated by the dist-info entry.
