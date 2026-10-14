# python-nvidia-cutlass-dsl-libs-cu13

## Build requirements
- The package is installed from the upstream wheel for the build architecture and the build's CPython, not built from the source tarball. The tarball is the whole cutlass repository and does not produce this artifact.
- Building from source would need a CUDA toolkit at build time, which openSUSE does not ship, and a pinned LLVM. That is why the wheel is used.
- Every enabled python flavor needs a matching per-CPython wheel. The upstream release ships one wheel per CPython version (including the free-threaded build) for each supported architecture, so the spec must pick the wheel that matches each flavor.
- Runtime Requires come from the wheel's metadata, not hand-written.
Checked against 4.7.1 on openSUSE_Tumbleweed, x86_64.

## Workarounds
- Source fetch replaced by the upstream wheel, because the source tree cannot build this package and needs CUDA at build time (see above).
- The wheel's dist-info LICENSE is excluded from the directory glob in %files and tagged with %license instead.
Checked against 4.7.1 on openSUSE_Tumbleweed, x86_64.

## Failure history
- Spec left TODO and FIXME placeholders (license, summary, description, Requires, %files): fixed by filling them from the wheel's metadata and contents.
- File-list check reported the __pycache__ directory under the _mlir_libs package directory as unowned, although the build succeeded: fixed by owning that directory with a %dir entry.
Checked against 4.7.1 on openSUSE_Tumbleweed, x86_64.
