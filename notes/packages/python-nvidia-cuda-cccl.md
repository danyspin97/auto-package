# python-nvidia-cuda-cccl notes

## Build requirements
- The upstream wheel declares no runtime dependencies (no Requires-Dist), so the package adds no Requires.
- The upstream wheel is platform-tagged even though it ships only headers and CMake files. Treat it as containing no binaries when choosing the package architecture.
Checked against 13.3.4.3.1 on openSUSE_Tumbleweed, x86_64.

## Test changes
- No upstream test suite is run. %check is a payload check that confirms the CCCL headers and CMake config are installed.
Checked against 13.3.4.3.1 on openSUSE_Tumbleweed, x86_64.

## Workarounds
- The full CCCL header tree (thrust and cuda headers included) and the CMake config are shipped, not trimmed, because removing them would drop library contents. The headers and CMake config go in a devel subpackage.
- The package is marked noarch because it has no compiled binaries. rpmlint reports the no-binary case as an error on the Python subpackages, and its badness score can exceed the limit otherwise.
Checked against 13.3.4.3.1 on openSUSE_Tumbleweed, x86_64.

## Failure history
- rpmlint no-binary error on both Python subpackages, badness above threshold: marked noarch and moved headers and CMake config to the devel subpackage. The build then succeeded.
  Checked against 13.3.4.3.1 on openSUSE_Tumbleweed, x86_64.
