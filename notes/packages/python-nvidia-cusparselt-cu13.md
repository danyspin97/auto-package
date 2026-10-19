# python-nvidia-cusparselt-cu13 notes

## Workarounds

- rpmlint RUNPATH: the bundled cuSPARSELt shared library in the upstream wheel for the build architecture has a RUNPATH of `$ORIGIN:`. rpmlint reports binary-or-shlib-defines-rpath at error level once per Python flavour, and those errors alone exceed the abort threshold of 999. The message is filtered in the rpmlintrc.
- Bundled libraries under sitelib are excluded from automatic Provides, so they are not advertised as system libraries. The libcuda.so.1 dependency is excluded from Requires, because the GPU driver supplies it at runtime rather than the build.

Checked against 0.8.1 on openSUSE_Tumbleweed, x86_64.

## Failure history

- rpmlint aborted the build because the RUNPATH errors pushed badness past 999. Fixed by filtering the RUNPATH message in the rpmlintrc; the build passes with it.
- An rpmlintrc filter with escaped dots was reported as unused, so the RUNPATH errors still aborted the build. Fixed by using an unescaped pattern that matches the message text.

Checked against 0.8.1 on openSUSE_Tumbleweed, x86_64.
