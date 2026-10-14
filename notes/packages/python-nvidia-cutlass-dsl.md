# python-nvidia-cutlass-dsl notes

## Build requirements
- Requires the nvidia-cutlass-dsl-libs-base and nvidia-cutlass-dsl-libs-cu12 packages at the same version. The compiled MLIR and CuTe libraries ship in these packages, not in the source tree. The cu13 libs package is needed only under the cu13 extra.
- Checked against 4.7.1 on openSUSE_Tumbleweed, x86_64.

## Test changes
- %check only verifies the package metadata; the test suite is not run. It runs from a fresh directory so the source tree is not imported in place of the installed package.
- Checked against 4.7.1 on openSUSE_Tumbleweed, x86_64.

## Workarounds
- Package the upstream wheel published for the release, not the source tarball. The CuTeDSL tree in the tarball cannot produce this package: its version file is written only by the editable-install preparation script, and its library directory is filled from the upstream wheel.
- Do not build the root pyproject.toml. It belongs to a different project (nvidia-cutlass) and would install the wrong artifact.
- Checked against 4.7.1 on openSUSE_Tumbleweed, x86_64. The source-tree finding came from reading the source before the build; the wheel source is the one that built.

## Failure history
- Spec still contained TODO placeholders (attempt 1): replaced with a complete wheel-based spec, including the Requires, the %check and the exact %files list.
- Spec built the root project from the tarball, producing the wrong artifact: switched to the upstream wheel.
- Checked against 4.7.1 on openSUSE_Tumbleweed, x86_64.
