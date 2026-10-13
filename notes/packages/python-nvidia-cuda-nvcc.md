# python-nvidia-cuda-nvcc

Prebuilt CUDA nvcc wheel, packaged from its unpacked layout. Last built: 13.4.92.

## Build requirements
- Import name is `nvidia`, a namespace package. Installed to sitelib.
- Requires: `nvidia-nvvm`, `nvidia-cuda-runtime`, `nvidia-cuda-crt`. These are the wheel's runtime dependencies.
- BuildRequires: the same three packages. The reason for the BuildRequires is not recorded; it is likely so %check can import the package.

## Test changes
- %check imports `nvidia.cu13` and checks that nvcc is executable, once per flavor.

## Workarounds
- Headers are removed from the package, and their RECORD lines are dropped from the dist-info. The reason is not recorded.
- The license is left out of the dist-info claim and is listed with %license instead.
