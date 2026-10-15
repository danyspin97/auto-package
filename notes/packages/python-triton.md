# python-triton

## Build requirements
- Build from the upstream wheel for the build architecture, not from source. The source build needs NVIDIA CUDA headers (cuda.h, cupti.h) that the sdist does not vendor and that the download step cannot fill in offline. It also needs an LLVM pinned to a fork commit that distro LLVM is unlikely to match. No distro package provides the CUDA headers.
Checked against 3.8.0 on openSUSE_Tumbleweed, x86_64.
- Upstream publishes one wheel per CPython ABI for each architecture. The pipeline should check that every enabled %python flavour has a published wheel for the build architecture. The free-threaded (cp314t) wheel is not a standard flavour.
Checked against 3.8.0 on openSUSE_Tumbleweed, x86_64 (cp313 and cp314 wheels); cp314t against 3.7.1.

## Workarounds
- The wheel installs the proton and proton-viewer console scripts into /usr/bin in every flavour. Package them in each flavour's %files with the python_alternative pattern.
Checked against 3.8.0 on openSUSE_Tumbleweed, x86_64.
- Bundled plugin libraries carry build-tree RUNPATHs, and the wheel ships static archives. Strip the RUNPATHs and delete the static archives so rpmlint passes.
Checked against 3.8.0 on openSUSE_Tumbleweed, x86_64.
- Zero-length placeholder modules trigger rpmlint zero-length errors. Filter them in the rpmlintrc. Each filter needs a leading wildcard: unanchored filters were reported unused while their matching errors were printed. Bundled backend headers trigger devel-file-in-non-devel-package at low badness and did not block the build.
Checked against 3.8.0 on openSUSE_Tumbleweed, x86_64.

## Failure history
- Proton scripts installed to /usr/bin but not in %files: "installed but unpackaged" error. Fixed by packaging them per flavour.
- rpmlint badness over threshold from RUNPATHs on the bundled plugin libraries and from the static archives. Fixed by stripping the RUNPATHs and deleting the static archives.
- rpmlintrc filters without a leading wildcard reported unused. Fixed by anchoring the filters.
Checked against 3.8.0 on openSUSE_Tumbleweed, x86_64.
