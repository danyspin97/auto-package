# python-triton

## Build requirements
- Build from the upstream wheel for the build architecture, not from source. The source tree's C++ includes cuda.h, cupti.h and cupti_pcsampling.h, which the NVIDIA driver and proton code compile against, and openSUSE ships no CUDA headers, so the extension cannot compile from source.
- Upstream publishes one wheel per CPython ABI for each architecture. Check that every enabled %python flavour has a published wheel for the build architecture. The free-threaded ABI is not used by any standard flavour.
- The source build also downloads a prebuilt LLVM pinned to a fork commit that distro LLVM does not match. Checked against 3.7.1 only.
Checked against 3.8.0 on openSUSE_Tumbleweed, x86_64 (wheel rule and CUDA headers).

## Workarounds
- Bundled plugin libraries carry build-tree RUNPATHs, and the wheel ships static archives for cupti and nvperf host. Set each RUNPATH to $ORIGIN instead of leaving the build path, and delete the static archives.
- Zero-length placeholder modules, and the driver.c and compile.c sources that are compiled at run time, trigger rpmlint errors. Filter them in the rpmlintrc. Each filter needs a leading wildcard: unanchored filters were reported unused while their matching errors were printed.
- The wheel installs the proton and proton-viewer console scripts into /usr/bin in each flavour. Package them in each flavour's %files with the python_alternative pattern.
Checked against 3.8.0 on openSUSE_Tumbleweed, x86_64 (RUNPATH, static archive and rpmlintrc filters). The proton script packaging is from 3.7.1.

## Failure history
- Proton scripts installed to /usr/bin but not in %files: "installed but unpackaged" error on 3.7.1. Fixed by packaging them per flavour.
- rpmlint badness over threshold from build-tree RUNPATHs and static archives: 60411 on 3.7.1, 20815 on 3.8.0. 3.7.1 fixed by stripping RUNPATHs and deleting the archives; 3.8.0 set $ORIGIN RUNPATHs, deleted the archives and added the rpmlintrc filters, and the build then passed.
- rpmlintrc filters without a leading wildcard were reported unused on 3.8.0. Fixed by adding the leading wildcard.
- On 3.7.1, the rpmlintrc filters for missing-hash-section and library-without-ldconfig were reported unused; cause not established.
Checked against 3.8.0 on openSUSE_Tumbleweed, x86_64 (badness and filter entries). The proton and missing-hash entries are from 3.7.1.
