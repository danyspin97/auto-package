# python-nvidia-cutlass-dsl-libs-core

Package for NVIDIA CUTLASS DSL libs core, version 4.7.1.

## Build requirements

- The package is built from the upstream wheel for the build architecture, not from the source checkout. The release has no sdist, and the checkout's root pyproject builds a different project (nvidia-cutlass 4.2.0.0). The wheel is pure Python, so the package is noarch.
- The wheel installs cutlass under the nvidia_cutlass_dsl directory's dsl_packages subdirectory, not at top level. %files owns that path.
- The wheel does not ship cutlass._mlir. It comes from the libs-base sibling package, so importing cutlass fails without it.
- Runtime dependencies from the release metadata are Requires, with their version bounds.
- backports.strenum is only required on Python below 3.11; keep its marker.

Checked against 4.7.1 on openSUSE_Tumbleweed, x86_64.

## Test changes

- No upstream test suite is run. The test/ trees in the checkout belong to the CUTLASS monorepo and need torch and an importable cutlass, so they do not apply to this package.
- %check byte-compiles the installed cutlass tree instead of importing it, because importing needs cutlass._mlir from the libs-base sibling.

Checked against 4.7.1 on openSUSE_Tumbleweed, x86_64.

## Workarounds

- The dist-info LICENSE is excluded from the %files glob and tagged %license separately, so it is owned once.

Checked against 4.7.1 on openSUSE_Tumbleweed, x86_64.

## Failure history

- Empty License tag broke rpmspec parsing; the tag must hold a non-empty expression.
- Source-build recipe built nvidia-cutlass 4.2.0.0 from the checkout instead of this package, and %check failed on missing torch and cutlass. Fixed by building the upstream wheel.
- Spec was left with unfinished placeholders. Fixed by completing the license, dependencies, check and file list.
- %files and the check assumed a top-level cutlass directory. Fixed by owning the dsl_packages path.
- Import check failed with missing cutlass._mlir, which the wheel does not ship. Fixed by byte-compiling the installed tree in %check instead of importing it.

Checked against 4.7.1 on openSUSE_Tumbleweed, x86_64.
