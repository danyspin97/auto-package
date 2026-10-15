# python-cuda-bindings

## Build requirements
- The package is built from the upstream wheel for the build architecture, not from source. A source build compiles Cython extensions that include cuda.h and link cudart_static, and the build hook stops unless CUDA_PATH or CUDA_HOME is set. openSUSE does not package the CUDA toolkit, so the wheel is the build input.
- numpy is required at both build and run time. The nvml, cudla, cufile and v2 nvrtc modules import numpy at module level, so importing the package fails without it.
- Upstream publishes one wheel per CPython version from cp310 to cp315, plus free-threaded cp314t and cp315t, each for manylinux x86_64 and aarch64. Choose the wheel that matches each CPython flavor the project builds.
- NVIDIA driver libraries are not provided by the distro and are excluded from automatic Requires; they come from the installed driver at run time.
Checked against 13.4.4 on openSUSE_Tumbleweed, x86_64.

## Test changes
- %check does not run the upstream test suite. It imports each cuda.bindings module (driver, runtime, nvrtc, nvml, nvjitlink, nvvm, nvfatbin), so a broken extension fails the build. The reason the suite is skipped is not recorded.
Checked against 13.4.4 on openSUSE_Tumbleweed, x86_64.

## Workarounds
- The upstream wheels are used as the build input instead of building from source, because the CUDA toolkit a source build needs is not packaged (see the first build requirement).
Checked against 13.4.4 on openSUSE_Tumbleweed, x86_64.

## Failure history
- The import check failed with "No module named numpy" because the extensions import numpy at load time. Fixed by adding numpy to BuildRequires and Requires.
Checked against 13.4.4 on openSUSE_Tumbleweed, x86_64.
