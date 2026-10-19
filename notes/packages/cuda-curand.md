# cuda-curand

## Build requirements
- patchelf is a BuildRequires. It strips the RUNPATH from the shared libraries in %install (see Workarounds). Without that, rpmlint's binary-or-shlib-defines-rpath error fails the build.
Checked against 10.4.4.72 on openSUSE_Tumbleweed, x86_64.

## Test changes
- None. The package is a prebuilt upstream archive with no test suite, so the spec has no %check and nothing is deselected or patched.
Checked against 10.4.4.72 on openSUSE_Tumbleweed, x86_64.

## Workarounds
- The upstream shared libraries carry a RUNPATH. %install removes it with patchelf so rpmlint does not fail the build on it.
Checked against 10.4.4.72 on openSUSE_Tumbleweed, x86_64.

## Failure history
- rpmlint binary-or-shlib-defines-rpath error on the cuRAND shared libraries aborted the build. Fixed by stripping the RUNPATH with patchelf; the build then succeeded.
Checked against 10.4.4.72 on openSUSE_Tumbleweed, x86_64.
