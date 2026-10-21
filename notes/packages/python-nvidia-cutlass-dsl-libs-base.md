# python-nvidia-cutlass-dsl-libs-base

## Build requirements
- Requires nvidia-cutlass-dsl-libs-core at the same version as this package, so the shared nvidia_cutlass_dsl namespace resolves to one directory once both are installed into sitelib. It is a sibling wheel from the same upstream release, not part of the checked-out source tree, so it has to come from the package repositories.
- typing-extensions is in both BuildRequires and Requires, using the distro's package name, so the %check import smoke test can run.
- %files must match the wheel's real layout: the top-level package directory plus the dist-info directory. There is no package-named directory of its own.
Checked against 4.8.0 on openSUSE_Tumbleweed, x86_64 (the core sibling entry was last checked against 4.7.1).

## Test changes
- %check does not use the importlib PathFinder find_spec chain. That chain raised KeyError for nvidia_cutlass_dsl when run from the build tree, so it is now a plain import smoke test run from an empty directory.
Checked against 4.8.0 on openSUSE_Tumbleweed, x86_64.

## Workarounds
- Source: the upstream wheel for the build architecture, one per enabled CPython flavor, is used instead of the checked-out source. The release publishes wheels only, with no sdist. The checkout is NVIDIA's CUTLASS C++ tree; its pyproject declares a different project name and version, so the source tarball and the cutlass-%{version} unpack directory are the wrong project. Wheel checksums are pinned.
- License: the wheel ships proprietary NVIDIA binaries under the NVIDIA software license. The BSD-3 LICENSE in the C++ tree does not cover them, so the License tag is LicenseRef-NvidiaProprietary.
Checked against 4.8.0 on openSUSE_Tumbleweed, x86_64.

## Failure history
- Spec left TODO and FIXME placeholders, a summary-only description, and comment lines containing % characters: fixed by writing the License, summary and description, and removing the TODO and % lines and the unreferenced rpmlintrc source.
- "Installed (but unpackaged)" for /usr/bin/run-iket and for the __pycache__ directories under _mlir/ and _mlir/dialects/: each flavor's install writes the same unversioned run-iket, so the second overwrites the first. Fixed by cloning run-iket per flavor with update-alternatives and claiming the __pycache__ directories in %files.
- %check KeyError for nvidia_cutlass_dsl from the PathFinder chain: fixed by the plain import smoke test described under Test changes.
Checked against 4.8.0 on openSUSE_Tumbleweed, x86_64 (the run-iket and __pycache__ entry was last checked against 4.7.1).
