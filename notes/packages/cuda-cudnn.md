# cuda-cudnn

## Build requirements
- patchelf is needed at build time to clear the RUNPATH of the vendored cuDNN shared libraries. On openSUSE_Tumbleweed it is in the OSS main repository.
- The source is the upstream NVIDIA cuDNN archive for the build architecture. Its sha256 is pinned in the spec.
Checked against 9.27.0.42 on openSUSE_Tumbleweed, x86_64.

## Workarounds
- Vendored shared libraries that carry RUNPATH $ORIGIN fail rpmlint's binary-or-shlib-defines-rpath check. %install strips the RUNPATH from the cuDNN libraries to pass it.
- The no-binary rpmlintrc filter was removed, because rpmlint reported it as unused once the RUNPATH was stripped.
Checked against 9.27.0.42 on openSUSE_Tumbleweed, x86_64.

## Failure history
- rpmlint badness 100001 exceeded threshold 999 from ten binary-or-shlib-defines-rpath errors on the cuDNN libraries, aborting the build at the rpmlint stage. Fixed by stripping RUNPATH in %install and dropping the unused no-binary filter; the build then succeeded on openSUSE_Tumbleweed, x86_64.
Checked against 9.27.0.42 on openSUSE_Tumbleweed, x86_64.
