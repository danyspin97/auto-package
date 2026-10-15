# python-onnxruntime notes

## Build requirements
- The spec repackages the upstream binary wheel for the build architecture and CPython version, not a source build. The upstream release for this version ships only wheels, with no sdist, and the source tree cannot produce the compiled extension without the separate CI build (CMake with network-fetched dependencies), which is out of scope for the spec.
- Wheels are per CPython version, including free-threaded builds, so the spec must select the wheel matching the Python version being built. Verify the wheel's published checksum in the spec.
Checked against 1.26.0 on openSUSE_Tumbleweed, x86_64.

## Workarounds
- Source-tree build replaced by the upstream wheel: setup.py only copies a prebuilt extension and writes build info into the package, so the sdist cannot be built into a wheel by itself. Reason: wrong-artifact case, not a compiler or CUDA problem.
Checked against 1.26.0 on openSUSE_Tumbleweed, x86_64.

## Failure history
- Wheel-based spec had TODO placeholders for %files, runtime Requires and the onnxruntime_test script: filled them in, and the build succeeded.
Checked against 1.26.0 on openSUSE_Tumbleweed, x86_64.
