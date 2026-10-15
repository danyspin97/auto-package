# python-nvidia-cusparselt-cu13 notes

## Workarounds

- rpmlint: the upstream wheel's libcusparseLt shared library has a RUNPATH of `$ORIGIN:`. rpmlint reports it as binary-or-shlib-defines-rpath at error badness, once per Python flavour, and that is enough to exceed the abort threshold. It is filtered in the rpmlintrc, which keeps the build from aborting on it.
- Packaging: the licence is set to SUSE-NonFree, bundled libraries are excluded from Provides, and the licence file is marked with %license. These come from the spec review, not from a build error.

Checked against 0.8.1 on openSUSE_Tumbleweed, x86_64.

## Failure history

- rpmlint aborted the build because the RUNPATH errors pushed the badness past the threshold of 999. Fixed by adding the RUNPATH filter to the rpmlintrc.
- The first filter used escaped dots, so rpmlint reported it as unused and the RUNPATH errors still aborted the build. Fixed by using an unescaped pattern that matches the message text.

Checked against 0.8.1 on openSUSE_Tumbleweed, x86_64.
