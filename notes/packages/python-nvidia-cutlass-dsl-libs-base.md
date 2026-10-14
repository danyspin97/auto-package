# python-nvidia-cutlass-dsl-libs-base

## Build requirements
- Requires nvidia-cutlass-dsl-libs-core at the same version as this package. It is a sibling wheel from the same upstream release, not part of the checked-out source tree, so it has to come from the package repositories.
- %files must match the wheel's real layout: the top-level package directory plus the dist-info directory. There is no package-named directory of its own.
Checked against 4.7.1 on openSUSE_Tumbleweed, x86_64.

## Workarounds
- Source: the upstream wheel for the build architecture is used instead of the checked-out source. The release publishes wheels only, with no sdist, and the checkout contains no project of this name, so the source tarball and the `cutlass-%{version}` unpack directory point at the wrong project. The wheel's checksum is pinned.
- License: the wheel ships a proprietary NVIDIA license, so the License tag uses LicenseRef-NvidiaProprietary.
Checked against 4.7.1 on openSUSE_Tumbleweed, x86_64.

## Failure history
- Spec left TODO and FIXME placeholders, and comment lines containing % characters: replaced the License FIXME with the proprietary LicenseRef, and removed the TODO and % lines.
- Unversioned /usr/bin/run-iket master link reported as installed but unpackaged: planned fix is to claim it in the primary flavor's %files. Not confirmed by this build's notes.
Checked against 4.7.1 on openSUSE_Tumbleweed, x86_64.
