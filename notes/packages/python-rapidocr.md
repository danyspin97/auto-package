# python-rapidocr notes

## Build requirements
- The setuptools_scm build requirement is spelled with the underscore form of the name, as the distro packages it.
- The onnxruntime engine is optional upstream, so the spec Recommends it rather than Requires it.
- The rtl extra's dependency is optional and is only Recommended, for right-to-left text support.
Checked against 3.10.0 on openSUSE_Tumbleweed, x86_64.

## Test changes
- %check runs only tests/test_rtl_dependency.py, the one test file that imports nothing beyond the image and numeric libraries, so it runs offline. The other tests are not run.
Checked against 3.10.0 on openSUSE_Tumbleweed, x86_64.

## Workarounds
- PRETEND_VERSION is set because the source tarball has no git metadata for setuptools_scm to read the version from.
- The project is in the python/ subdirectory of the source tree, so setuptools_scm is pointed at the parent directory as the repository root.
- Runtime dependencies come from requirements.txt in python/, because the project declares them as dynamic; the spec must keep that file as the source of truth.
- The rapidocr console script is wired through the alternatives mechanism in the spec.
- %license entries name files that exist in the source tree.
Checked against 3.10.0 on openSUSE_Tumbleweed, x86_64.
