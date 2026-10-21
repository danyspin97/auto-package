# python-nvidia-cutlass-dsl-libs-core

Package for NVIDIA CUTLASS DSL libs core, version 4.8.0.

## Build requirements

- The package is built from the upstream wheel for the build architecture. The wheel is pure Python, so the package is noarch.
- The wheel installs only under the nvidia_cutlass_dsl directory's dsl_packages subdirectory, and %files owns that path. The nvidia_cutlass_dsl and dsl_packages directories have no __init__.py.
- The wheel does not ship cutlass._mlir. It comes from the libs-base sibling, so importing cutlass fails without it.
- Runtime dependencies from the wheel metadata are Requires, with their version bounds.
- backports.strenum is only required on Python below 3.11: keep its marker, and write the conditional Requires in parentheses so OBS does not read "if" as a package name.
- The sass-extra extra is a Recommends, not a Requires.
- License is SUSE-NonFree: the CuTeDSL pyproject marks the NVIDIA wheel LicenseRef-NvidiaProprietary.

Checked against 4.8.0 on openSUSE_Tumbleweed, x86_64.

## Test changes

- No upstream test suite is run. The test/ trees belong to the CUTLASS monorepo and need torch and an importable cutlass, so they do not apply to this package.
- %check byte-compiles the installed cutlass tree instead of importing it or probing it with importlib, because importing needs cutlass._mlir and a find_spec probe of the namespace packages raises KeyError.

Checked against 4.8.0 on openSUSE_Tumbleweed, x86_64.

## Workarounds

- The source checkout cannot build this package: its root and python/CuTeDSL packaging build nvidia-cutlass and nvidia-cutlass-dsl. The spec uses the upstream wheel instead.
- The license file inside the dist-info directory is excluded from the %files glob and tagged %license separately, so it is owned once.

Checked against 4.8.0 on openSUSE_Tumbleweed, x86_64.

## Failure history

- Empty License tag broke rpmspec parsing; fixed by giving a non-empty expression (SUSE-NonFree).
- Building the checkout produced nvidia-cutlass 4.2.0.0 and %check failed on missing torch and cutlass; fixed by using the upstream wheel.
- Unfinished placeholders (FIXME license, TODO %check) left in the spec; fixed by completing the license, dependencies, check and file list.
- %files and %check assumed a top-level cutlass directory; fixed by owning the dsl_packages path.
- Import check failed on missing cutlass._mlir; fixed by byte-compiling the installed tree.
- %check's find_spec probe of nvidia_cutlass_dsl.dsl_packages raised KeyError because those are namespace directories; fixed by byte-compiling the installed tree instead.

Checked against 4.7.1 and 4.8.0 on openSUSE_Tumbleweed, x86_64.
