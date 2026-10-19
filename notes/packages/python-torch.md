# python-torch notes

## Build requirements
- The NVIDIA runtime wheels that torch loads at import time (cublas, cudnn, nccl, nvrtc, cudart, cufft, curand, nvjitlink, cusparse, cusolver, nvshmem and others) are BuildRequires for %check and Requires of the package. Their sonames are excluded from automatic requires.
- Where the upstream wheel for the build architecture is CUDA-enabled, the cuda-toolkit wheel does not pull in every component, so the components it omits are listed explicitly.

Checked against 2.14.1 on openSUSE_Tumbleweed, x86_64.

## Test changes
- %check only imports functorch and runs no test suite. The reason for skipping the suite is not recorded.

Checked against 2.14.1 on openSUSE_Tumbleweed, x86_64.

## Workarounds
- The sdist does not contain the git submodule contents under third_party/ (pybind11, fmt, sleef, protobuf, XNNPACK, FP16 and others are empty), and %license names about 150 LICENSE files there. The package therefore builds from the upstream wheel matching the build architecture and CPython version, not from source.

Checked against 2.14.1 on openSUSE_Tumbleweed, x86_64.

## Failure history
- Source build: empty third_party submodules and missing %license paths → switched to the upstream wheel.
- %check import: torch's import-time CUDA preload did not find the NVIDIA runtime libraries → added them to BuildRequires and Requires.
- rpmlint: badness 3833 exceeded the threshold of 999 → dropped C++ template files from the package, removed shebangs from non-executable library modules, and removed rpmlintrc filters that matched nothing.

Checked against 2.14.1 on openSUSE_Tumbleweed, x86_64.
