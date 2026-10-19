# python-cuda-bindings

## Build requirements
- numpy is needed at build and run time. The nvml, cudla and cufile modules import numpy at module level, so importing the package fails without it.
- Upstream publishes one wheel per CPython version from cp310 to cp315, plus free-threaded cp314t and cp315t, for each manylinux architecture. Pick the wheel that matches each CPython flavor the project builds.
- NVIDIA driver libraries are not provided by the distro and are excluded from automatic Requires; they come from the installed driver at run time.
Checked against 13.4.4 on openSUSE_Tumbleweed, x86_64.

## Test changes
- %check does not run the upstream test suite. It imports each cuda.bindings module (driver, runtime, nvrtc, nvml, nvjitlink, nvvm, nvfatbin), so a broken extension fails the build. The reason the suite is skipped is not recorded.
Checked against 13.4.4 on openSUSE_Tumbleweed, x86_64.

## Workarounds
- The upstream wheel for the build architecture is the build input instead of a source build. A source build compiles Cython extensions against the CUDA toolkit headers (cuda.h), and the build hook fails unless CUDA_PATH or CUDA_HOME points at a toolkit, which openSUSE does not package.
- The toolkit is needed only at build time, not at run time.
Checked against 13.4.4 on openSUSE_Tumbleweed, x86_64.

## Failure history
- The import check failed with "No module named numpy" because nvml imports numpy at load time. Fixed by adding numpy to BuildRequires and Requires; the build passes with it.
Checked against 13.4.4 on openSUSE_Tumbleweed, x86_64.
