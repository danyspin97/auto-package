# python-nvidia-nvshmem-cu13 notes

## Build requirements
- The package is a prebuilt upstream wheel with no compile step; the spec installs the wheel for the build architecture.
- The wheel's WHEEL file sets Root-Is-Purelib to true, so the files install to the site-packages directory, not the platlib one.
- The License tag is SUSE-NonFree, because the upstream license is proprietary.
- libcuda.so.1 is excluded from automatic Requires, because the NVIDIA driver provides it at run time and it is not a package we can depend on.
- Driver sonames and optional-plugin sonames are excluded from automatic Requires too, so the package does not require libraries it does not need to load.
Checked against 3.4.5 on openSUSE_Tumbleweed, x86_64.

## Test changes
- No tests are deselected or patched. %check only asserts that the host library soname exists in each flavor, and it passes.
Checked against 3.4.5 on openSUSE_Tumbleweed, x86_64.

## Workarounds
- The bundled nvshmem headers and the static device library are deleted in %install, so the package ships runtime libraries only and no include or static-library filter is needed.
- %files lists the exact paths from the wheel's RECORD, minus the deleted headers and static library. Directories in the shared nvidia namespace are listed with %dir, because other NVIDIA packages share them.
- The license file is excluded from the install and then tagged as %license.
- rpmlintrc filters binary-or-shlib-defines-rpath for the nvidia/nvshmem/lib directory. The upstream wheel's libraries carry the vendor's own RUNPATH, including a leftover build-host path, and they are not ours to relink.
- The changelog helper needs keyring credentials the build agent does not have, so the .changes entry is written by hand.
Checked against 3.4.5 on openSUSE_Tumbleweed, x86_64.

## Failure history
- rpmlint aborted the build: 22 binary-or-shlib-defines-rpath errors on the vendor RUNPATH pushed the badness to 240103 against a limit of 999. Fixed by filtering that rule for the library directory (see Workarounds).
- The static device library raised devel-file-in-non-devel-package and static-library-without-debuginfo at 50 each. Fixed by deleting the library from the package rather than filtering the two rules.
Checked against 3.4.5 on openSUSE_Tumbleweed, x86_64.
