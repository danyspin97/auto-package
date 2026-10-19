# cuda-nvjitlink

## Build requirements
- Source0 is the upstream prebuilt archive for the build architecture, named with the package version and shipped next to the spec. The package repackages it rather than compiling anything, so the archive must be present in the package directory.

Checked against 13.4.92 on openSUSE_Tumbleweed, x86_64.

## Test changes
- The prebuilt package has no test suite, so no tests are run, deselected, ignored or patched. Found by reading the spec before the build (medium confidence).

Checked against 13.4.92 on openSUSE_Tumbleweed, x86_64.

## Workarounds
- The rpmlint configuration has three filters, each tied to a cause documented in its comments. They suppress lint findings only and do not change the build.

Checked against 13.4.92 on openSUSE_Tumbleweed, x86_64.
