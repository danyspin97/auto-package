# python-triton

## Build requirements
- Build from the upstream wheel for the build architecture, not from source. The source build downloads CUDA headers (nvcc, cudart, cupti, crt) and a prebuilt LLVM pinned to a fork commit at build time. The C++ code includes cuda.h and cupti.h, which the sdist does not vendor and no distro package provides. Setting the offline switch leaves those headers missing.
- Upstream publishes one wheel per CPython ABI: cp310 through cp314 (standard) and cp314t (free-threaded), for x86_64 and aarch64. The pipeline should check that every enabled %python flavour has a published wheel. The free-threaded wheel is not a standard flavour.
Checked against 3.7.1 on openSUSE_Tumbleweed, x86_64.

## Workarounds
- The wheel installs the proton and proton-viewer console scripts into /usr/bin. Package them in each flavour's %files with the python_alternative pattern.
- rpmlint flags build-tree RUNPATHs on the bundled plugins and shipped static archives. Strip the RUNPATHs and delete the static archives.
- rpmlint flags zero-length placeholder modules (make_launcher.py, gluon/_compiler.py). Filter them. The rpmlintrc filters for other rpmlint messages were reported unused in the failing run, so do not rely on them.
Checked against 3.7.1 on openSUSE_Tumbleweed, x86_64.

## Failure history
- Proton scripts installed to /usr/bin but not in %files: "installed but unpackaged" error. Fixed by packaging them per flavour.
- rpmlint badness 60411 over threshold 999, after %check passed: fixed by stripping plugin RUNPATHs, deleting static archives and filtering the zero-length modules.
Checked against 3.7.1 on openSUSE_Tumbleweed, x86_64.
