# python-nvidia-cutlass-dsl-libs-base

## Build requirements
- Requires nvidia-cutlass-dsl-libs-core at the same version as this package, so the shared nvidia_cutlass_dsl namespace resolves to one directory once both are installed into sitelib. It is a sibling wheel from the same upstream release, not part of the checked-out source tree, so it has to come from the package repositories.
- %files must match the wheel's real layout: the top-level package directory plus the dist-info directory. There is no package-named directory of its own.
Checked against 4.7.1 on openSUSE_Tumbleweed, x86_64.

## Workarounds
- Source: the upstream wheel for the build architecture, one per enabled CPython flavor, is used instead of the checked-out source. The release publishes wheels only, with no sdist. The checkout is NVIDIA's CUTLASS C++ tree (its pyproject declares a different project name and version), so the source tarball and the `cutlass-%{version}` unpack directory are the wrong project. Wheel checksums are pinned.
- License: the wheel ships a proprietary NVIDIA license, so the License tag uses LicenseRef-NvidiaProprietary.
Checked against 4.7.1 on openSUSE_Tumbleweed, x86_64.

## Failure history
- Spec left TODO and FIXME placeholders, a summary-only description, and comment lines containing % characters: replaced the License FIXME, wrote a real summary and description, and removed the TODO and % lines and the unreferenced rpmlintrc source.
- "Installed (but unpackaged)" for /usr/bin/run-iket and for the __pycache__ directories under _mlir/ and _mlir/dialects/: each flavor's install writes the same unversioned run-iket, so the second flavor overwrites the first. Fix: clone run-iket per flavor with update-alternatives, and claim the __pycache__ directories in %files.
Checked against 4.7.1 on openSUSE_Tumbleweed, x86_64.
