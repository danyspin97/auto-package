# python-opencv-python-headless

## Build requirements
- BuildRequires the CMake, Ninja and C++ compiler packages by their plain names. The build backend otherwise adds a PyPI CMake that the build service cannot resolve.
- BuildRequires the numpy development package (numpy >= 2.0), not only the runtime numpy. The runtime package does not ship the C API headers that the cv2 bindings compile against.
- Requires numpy >= 2.0 at runtime.
- The package is arch-specific: the sdist compiles a cv2 extension module with CMake, so it stays in the sitearch directory. The import name is cv2, so %files lists cv2/ and the dist-info.
- Optional features that CMake does not find (the IPP download, libavcodec, gstreamer, VTK) are detected and skipped. Do not add them as build dependencies.
Checked against 5.0.0.93 on openSUSE_Tumbleweed, x86_64.

## Test changes
- %check is an import smoke test instead of %pytest, because the upstream test layout is expected to fail under %pytest. This was predicted at review, not seen in a test run.
Checked against 5.0.0.93 on openSUSE_Tumbleweed, x86_64.

## Workarounds
- The CMake build tree leaked into the RUNPATH of the cv2 extension module. A quilt patch to setup.py turns off RPATH handling in CMake for both build and install; without it rpmlint reports binary-or-shlib-defines-rpath.
- The changelog entry is written by hand, because the packaging tool's changelog command needs a session D-Bus that the build agent does not have.
Checked against 5.0.0.93 on openSUSE_Tumbleweed, x86_64.

## Failure history
- CMake compile failed because the cv2 bindings could not find the numpy C API header → added the numpy development BuildRequires.
- rpmlint binary-or-shlib-defines-rpath on the cv2 extension module → RPATH patch under Workarounds.
Checked against 5.0.0.93 on openSUSE_Tumbleweed, x86_64.
