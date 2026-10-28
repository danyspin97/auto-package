---
name: nvidia-wheels
description: Use when the package or its sources involve NVIDIA CUDA — a build that mentions nvcc, cuda.h, CUDAToolkit or libcudart, an nvidia-<lib>-cuNN or cuda-* PyPI wheel, a wheel whose .so files link libcuda/libnccl/libcublas, .cu/.ptx/.cubin files, NVIDIA's split cutlass-dsl wheels, or a GPU-only test suite. Covers when to repackage the upstream wheel, the driver and vendor libraries that are never packages, headers, bundled sonames, SUSE-NonFree licensing, split wheels sharing one tree, and %check without a GPU.
---

# NVIDIA / CUDA packages

The CUDA-specific rules the numbered conventions only point at. Each section
keeps the condition it applies under; read the one that matches.

## Not Factory or SLE material

The output of `use_upstream_wheel` (a repackaged vendor wheel) and any package
whose `License:` contains `SUSE-NonFree` are not Factory or SLE material. Say so
in `reasoning`. They stay out of the openSUSE/SUSE OSS projects and are never
submitted there.

## CUDA at build time means: use the upstream wheel (convention 84)

openSUSE has no CUDA toolkit (`cuda-devel`/`cuda-runtime` have no PyPI package
and no public source), so a Python package whose BUILD needs CUDA can never be
compiled here. Repackage the upstream PyPI wheel instead (convention 28
Exception 4), in the FIRST review turn when the source already shows the need,
not after a failed build:

1. Look for a BUILD-time CUDA need: `find_package(CUDAToolkit ...)`,
   `enable_language(CUDA)`, `nvcc` in setup.py/CMake/Makefile, `.cu` files
   compiled by `ext_modules`/CMake (`CUDAExtension`), `#include <cuda*.h>` /
   `<cub/...>` / `<nccl.h>` in compiled sources, linking
   `libcudart`/`libcublas`/`libnccl`, or `cuda-devel`/`cuda-runtime` already in
   the spec's BuildRequires.
2. If found: do NOT add `cuda-devel`/`cuda-runtime` and do NOT report them in
   `unresolvable_packages`. Check the published wheels (WebFetch
   `https://pypi.org/pypi/<name>/<version>/json`) and return action
   `"use_upstream_wheel"` with `why` naming what needs CUDA and the line that
   proves it (`find_package(CUDAToolkit REQUIRED)` failing, a literal `nvcc:
   command not found`). NVIDIA's `nvcc` compiling `.cu` device code
   (`nvidia-nccl-cu13`, `nvidia-nvshmem-cu13`, CUDA-CCCL and similar) is the
   standing case. If every wheel is locked to one CPython each (`cp313`,
   `cp314`, ...), follow convention 49 instead and return `"edited"`. If PyPI
   publishes no usable wheel at all, build from source and report the missing
   toolkit component once in `unresolvable_packages` under its redist name (an
   extra package the pipeline builds), quoting the build.log line that proves
   it is needed.
3. A package that merely DEPENDS at runtime on a CUDA-needing package is
   unaffected: it keeps its normal build and `Requires`; the dependency is built
   by this same rule (wheel) when the pipeline gets to it. CUDA Python packages
   that build without CUDA are reported in `unresolvable_packages` like any
   other missing dependency (convention 27), never as `cuda-devel`/`cuda-runtime`.
4. CUDA is NOT a build-time need when only used at import/run time: NVRTC /
   Triton / `torch.utils.cpp_extension` JIT-compiling `.cu` strings, or the
   `dlopen` shape below. Those build normally.

A build failing on `cuda.h`, `libcudart` or `nvcc` is never fixed by adding
`cuda-devel`/`cuda-runtime`: they cannot exist here.

CUDA Python ecosystem libraries (cuda-python's cuda.core / cuda.bindings /
cuda.pathfinder, nvmath-python, nvshmem4py, cuda-compute, numba-cuda-mlir,
cuda-tile) publish real source and the pipeline builds missing ones like any
other dependency, but only when their own build does not need the CUDA headers
or toolchain; `cuda-bindings` compiles against `cuda.h`, so it is repackaged from
its wheel. A binding to a vendor library (`cuda-bindings`, wrapping the CUDA
driver API) is still a normal Python dependency: keep it as `Requires` and
`BuildRequires` as far as needed (convention 61). A `cuda` namespace that
resolves without `cuda.bindings` while `python3X-cuda-bindings` is only a few KB
is an unfinished dependency spec (convention 61); do not guess at
`cuda-pathfinder` or at renamed modules.

**dlopen/dlsym at runtime is not a build-time dependency at all.** Some packages
use their own minimal, ABI-compatible struct/typedef stubs for CUDA/cuFile/NCCL
and load the real symbols with `dlopen`/`dlsym`, without `#include`ing the real
headers or linking `libcudart`/`libcufile`/`libnccl`. Grep the actual `#include`
targets and link libraries (`target_link_libraries` / `-lcudart` / CMake
`find_package(CUDAToolkit)`) before assuming CUDA references in the source mean
a build-time dependency: `dlopen("libcudart.so", ...)` needs neither
`cuda-devel` nor `cuda-runtime`. Reporting them anyway costs a huge pointless
CUDA toolkit build. This is not "working around" a dependency (convention 28's
exceptions): there never was one to report.

## noarch or not: CUDA kernels and compiled payloads

- **CUDA kernel packages are a common false positive for arch-specific**: a
  package can ship `.cu`/`.cpp` source files (or CUDA C++ in Python string
  literals) compiled at IMPORT or RUN time by NVRTC /
  `torch.utils.cpp_extension` / Triton's JIT, never during the RPM build. With no
  `ext_modules=`, CMakeLists.txt or cythonized `.pyx`, this is `BuildArch:
  noarch` despite the `.cu`/`.cpp` files (convention 3).
- A "pure Python" wheel can still ship compiled code (convention 60): look for
  `.so` files and CUDA sources or headers (`.cu`, `.cuh`, `.ptx`, `.cubin`,
  `.fatbin`), then `readelf -d <file.so>` for the NEEDED lines.

## Driver libraries are never packages

`libcuda.so.1` is the NVIDIA DRIVER API, supplied at runtime by the driver (not
the CUDA toolkit), so a package that links only against it can use the upstream
wheel here. Do not add `cuda-devel`/`cuda-runtime` or any CUDA toolkit
BuildRequires for it. RPM's automatic dependency generator turns that NEEDED
entry into `Requires: libcuda.so.1()(64bit)`, which no repository provides, so
every package depending on yours becomes unresolvable (`nothing provides
libcuda.so.1()(64bit)`). Filter it out in the package that ships the `.so`,
before `%description`. The pipeline already writes the one regex for every
driver soname (`libcuda`, `libnvidia-*`, `libnvcuvid`, `libcudadebugger`,
`libnvoptix`) into a compiled wheel's spec, and adds it itself when OBS reports
one unresolvable; fold any further pattern into that same line with `|` (rpm
honours one `__requires_exclude`), never a second line. Such a capability is
never a package: do NOT report it in `unresolvable_packages`, and do not add a
`Requires:` or `Provides:` for it. A dependency you only consume that carries it
is repaired by the pipeline; there is nothing for you to resolve.

Runtime CUDA libraries that the wheel loads through pip packages (`nvidia-*`,
`cuda-bindings`) belong in Requires, as the package's own metadata declares
them. Record what you found, with the wheel name, the NEEDED list and the
decision, in your reasoning so the package notes keep it.

## nvidia-<lib>-cuNN wheels and vendor libraries (convention 64)

NVIDIA's `nvidia-<lib>-cuNN` wheels (NCCL, cuDNN, cuBLAS, ...) are vendor
binaries carried through PyPI; they exist here only so a Python package that
loads them from site-packages (torch) has them, so depend on them by that PyPI
name and only then. Never report a vendor library (`libcuda`, `libnccl`,
`libcudnn`, `libcublas`, ...) as a package to build.

Binary-only upstream (convention 28 Exception 3) example: NVIDIA's CUTLASS
maintainers confirmed CuTeDSL's compiled artifacts (libCutlassIRPythonCAPI.so
and the `_cute*.so` extension modules) ship only in the PyPI wheel and will not
be open-sourced (https://github.com/NVIDIA/cutlass/issues/2297). Each NVIDIA
CUDA toolkit component is a prebuilt wheel with no sdist (convention 32, skill
prebuilt-wheel-packaging).

## Headers in NVIDIA wheels: delete them

NVIDIA's CUDA wheels bundle `include/*.h`. They look like a Python C-API's
public headers (convention 53's `-devel` split) but are not: delete `include/`
in `%install` instead of splitting a `-devel` subpackage
(`%python_expand rm -rf %{buildroot}%{$python_sitelib}/nvidia/*/include`). A
package that needs CUDA to build is repackaged from its wheel, so no build here
ever compiles against them.

## Bundled sonames in NVIDIA wheels (convention 68)

- For the sonames a wheel links against only through OPTIONAL plugins
  (NVSHMEM's MPI, UCX, fabric and PMIx transports: `libmpi`, `liboshmem`,
  `libpmix`, `libucp`, `libucs`, `libfabric`, `libmlx5`), add
  `%global __requires_exclude ^lib(mpi|oshmem|pmix|ucp|ucs|fabric|mlx5)\.so.*$`
  (folded into the one generated line with `|` when there is one): otherwise
  installing the package drags in OpenMPI and UCX for a feature nobody asked for.
- For a soname another package of this project supplies (`libnccl.so.2` from
  `python-nvidia-nccl-cu13`), exclude the auto-Requires and write the explicit
  `Requires: python-nvidia-nccl-cu13`, so the choice between flavors is not left
  to OBS (`have choice for libnccl.so.2`), and add it to `BuildRequires` too when
  OBS reports that choice.
- Delete the stubs of another architecture (`aarch64-unknown-linux-gnu` in an
  x86_64 package) along with headers, `*.a` archives and test libraries.

## Licence (convention 69)

A wheel that bundles NVIDIA's `ptxas`, `cuobjdump`, `nvdisasm`, `libcupti*`,
`libcudart` or `libcublas` adds `AND SUSE-NonFree` to `License:` (openSUSE's tag
for redistributable proprietary binaries). Every `LicenseRef-...` spelling,
`LicenseRef-NVIDIA-Proprietary` and `LicenseRef-NvidiaProprietary` included, is
`invalid-license` in rpmlint, and so is `NVIDIA Proprietary Software`. A vendored
`3rdparty/cutlass` tree adds its own licence. Such a package is not Factory or
SLE material (top of this skill).

## Split wheels sharing one tree (conventions 76, 83)

NVIDIA's `nvidia-cutlass-dsl-libs-base/-core/-cu12/-cu13` all fill
`nvidia_cutlass_dsl/dsl_packages/...` and the `cutlass` package:

- They must put it in the same directory: a `.pth` that inserts only the first
  namespace portion on `sys.path` sees `/usr/lib64/...` and never
  `/usr/lib/.../cutlass/__init__.py`. The symptom is `ModuleNotFoundError: No
  module named 'cutlass._mlir'` (or `No module named 'cutlass.cute'`, or `cannot
  import name ... (unknown location)`) although the files are installed. The
  `py3-none-any` wheel (`-core`, noarch) owns `__init__.py` and installs into
  sitelib, so `-base` (which holds `_mlir`) and `-cu13` move their trees (and
  dist-info) to sitelib in `%install`; fix all siblings in the project. The
  skeleton does this; do not undo it, do not add a second move, do not hide it
  with `PYTHONPATH` in `%check`.
- `Root-Is-Purelib: true` NVIDIA wheels stay in `%{python_sitelib}` with
  `ExclusiveArch` even though they ship `.so` files (they find their sibling
  libraries relative to each other inside one tree).
- The `-cu13` flavor's `%check` must not import a module whose other half
  (`..._mlir.ir`) lives in a wheel it does not `Requires:`: give it that
  `Requires:` and import the chain (`import cutlass.cutlass_dsl, cutlass._mlir`),
  not a `test -e` of one library. A downstream wheel (`tokenspeed-mla`) that
  fails to import because of a split wheel is fixed in the split wheel's spec
  (`patch_dependencies`, convention 62 case b), not by copying files.

## %check without a GPU (convention 67)

A test suite that needs CUDA (the whole of `flashinfer`'s `tests/`, a kernel
library's GPU tests) cannot pass on an OBS worker. Run the import smoke test
that needs no device plus the CPU-safe tests selected by a marker or directory
(`--ignore=tests/gpu`, `-m "not gpu"`). `%check` of a CUDA wheel that fails at
import for want of a device (`cannot import name 'ir' from _mlir`) checks the
installed files instead, quoting the import failure. Never size a skip list from
the word `cuda` alone (convention 81).

## Arch-limited dependencies

A `noarch` package that `Requires` a CUDA library (built only for some
architectures) propagates that library's `ExclusiveArch` (convention 85).
