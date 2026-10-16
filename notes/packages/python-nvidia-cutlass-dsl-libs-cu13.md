# python-nvidia-cutlass-dsl-libs-cu13

## Build requirements
- The package is installed from the upstream wheel for the build architecture and the build's CPython, not built from the source tarball. The tarball is the whole cutlass repository and does not produce this artifact.
- Building from source would need a CUDA toolkit at build time, which openSUSE does not ship, and a pinned LLVM. That is why the wheel is used.
- The root pyproject.toml of the source tarball builds the nvidia-cutlass project, not this one, so it must not be used as the build source.
- Every enabled python flavor needs a matching per-CPython wheel. The upstream release ships one wheel per CPython version (including the free-threaded build) for each supported architecture, so the spec must pick the wheel that matches each flavor.
- The wheel has native extensions and installs to the arch-specific site-packages directory. The pure-Python libs-core and libs-base siblings install to the pure-Python directory, and both trees share the nvidia_cutlass_dsl namespace.
- The libs-core and libs-base sibling packages must be installed in the build chroot, since they provide the pure-Python nvidia_cutlass_dsl tree.
- Runtime Requires come from the wheel's metadata, not hand-written.
Checked against 4.7.1 on openSUSE_Tumbleweed, x86_64.

## Test changes
- %check does not import the full cutlass package chain. The `ir` submodule comes only from the CUDA-built IR extension, which is not in the libs-base tree and cannot be produced without the CUDA environment, so that import fails on a clean install.
- %check instead checks the installed runtime library, the presence of the IR extension file and the version module, without needing a device.
Checked against 4.7.1 on openSUSE_Tumbleweed, x86_64.

## Workarounds
- Source fetch replaced by the upstream wheel, because the source tree cannot build this package and needs CUDA at build time (see above).
- The wheel's dist-info LICENSE is excluded from the directory glob in %files and tagged with %license instead.
Checked against 4.7.1 on openSUSE_Tumbleweed, x86_64.

## Failure history
- Spec left TODO and FIXME placeholders (license, summary, description, Requires, %files): fixed by filling them from the wheel's metadata and contents.
- File-list check reported the __pycache__ directory under the _mlir_libs package directory as unowned, although the build succeeded: fixed by owning that directory with a %dir entry.
- %check copied the libs-base tree from a hard-coded site path that was the wrong directory: fixed by not copying it.
- %check import of the cutlass package chain failed with an ImportError for `ir` from _cutlass_ir._mlir, even with the core, base and cu13 trees merged: fixed by narrowing %check to the device-free checks above.
Checked against 4.7.1 on openSUSE_Tumbleweed, x86_64.
