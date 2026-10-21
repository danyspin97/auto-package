# python-nvidia-cutlass-dsl-libs-cu12 notes

## Build requirements

- The package repackages the upstream binary wheel for the build architecture and CPython flavor, not a source build. The sdist's source tree builds a different distribution (`nvidia-cutlass`), and its CuTeDSL package downloads the published wheel instead of compiling the extension modules, so the compiled modules exist only in the wheel.
- The upstream project publishes one wheel per CPython flavor (cp310 to cp314, and cp314t) and per manylinux architecture (x86_64, aarch64), so the spec must select the wheel that matches the build's interpreter and architecture.
- Runtime Requires were filled in by hand and are not recorded here; check the spec. (Recorded against 4.7.1.)

Checked against 4.8.0 on openSUSE_Tumbleweed, x86_64.

## Workarounds

- The sdist is not used. The upstream wheel for the build architecture and flavor is repackaged instead, because the sdist's source tree does not produce this distribution's module names or its compiled CuTeDSL artifacts.
- The upstream issue cited as evidence that these artifacts ship only in the wheel was not checked; the decision rests on the source-tree evidence.

Checked against 4.8.0 on openSUSE_Tumbleweed, x86_64.

## Failure history

- Spec carried TODO/FIXME placeholders (licence, a description equal to the Summary, and %files). Fixed by setting the licence to BSD-3-Clause AND SUSE-NonFree (Python-side code is BSD-3-Clause; the bundled NVIDIA binaries fall under NVIDIA's EULA), writing a real description, and globbing the CPython tag of the extension modules in %files instead of hardcoding one flavor.
- On x86_64 only, the static archive and the aarch64 stub files were dropped from %files, since the x86_64 wheel does not install them.

Checked against 4.8.0 on openSUSE_Tumbleweed, x86_64.
