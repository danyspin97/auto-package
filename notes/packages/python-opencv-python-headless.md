# python-opencv-python-headless

## Build requirements
- BuildRequires the CMake, Ninja and C++ compiler packages by their plain names. The build backend otherwise adds a PyPI CMake that the build service cannot resolve.
- BuildRequires the numpy development package, not only the runtime numpy. The runtime package does not ship the C API headers that the cv2 bindings compile against.
- BuildRequires and Requires numpy >= 2.0. The upstream metadata asks for numpy>=2 on Python 3.9 and newer and numpy<2 on older Pythons, so this requirement only fits targets with Python 3.9 or newer.
- The upstream wheel for the build architecture is an abi3 platform wheel with Root-Is-Purelib false, and the sdist compiles a cv2 extension module. The package is therefore arch-specific and stays in the sitearch directory. The import name is cv2, so %files lists the cv2/ directory and the dist-info.
- Optional features that CMake does not find (the IPP download, libavcodec, gstreamer, VTK) are detected and skipped. Do not add them as build dependencies.
Checked against 4.13.0.92 on openSUSE_Tumbleweed, x86_64.

## Test changes
- %check is an import smoke test instead of %pytest, because the upstream test layout is expected to fail under %pytest. The upstream test layout itself was not run.
Checked against 4.13.0.92 on openSUSE_Tumbleweed, x86_64: the build passed with this check.

## Workarounds
- The spec excludes the bundled shared libraries from Provides, so the wheel's own copies are not advertised to other packages.
- The cv2 extension module can pick up the CMake build tree in its RUNPATH, which rpmlint reports as binary-or-shlib-defines-rpath. The intended fix is a patch to setup.py that turns off CMake's RPATH handling for build and install. No record shows it applied.
- The changelog entry is written by hand, because the packaging tool's changelog command needs a session D-Bus that the build agent does not have.
Checked against 4.13.0.92 on openSUSE_Tumbleweed, x86_64 (Provides exclusion); 5.0.0.93 on openSUSE_Tumbleweed, x86_64 (RPATH and changelog).

## Failure history
- CMake compile failed because the cv2 bindings could not find the numpy C API header → added the numpy development BuildRequires.
- rpmlint binary-or-shlib-defines-rpath on the cv2 extension module → RPATH patch planned under Workarounds; outcome not recorded.
Checked against 5.0.0.93 on openSUSE_Tumbleweed, x86_64.
