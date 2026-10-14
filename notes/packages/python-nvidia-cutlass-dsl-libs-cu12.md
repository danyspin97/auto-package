# python-nvidia-cutlass-dsl-libs-cu12 notes

## Build requirements

- The package is built from the upstream binary wheel for the build architecture, not from a source tarball. The distribution publishes no sdist, and the NVIDIA/cutlass source tree in the sdist builds a different distribution (`nvidia-cutlass`). Its compiled extension modules exist only in the wheel.
- Runtime Requires were filled in by hand for the prebuilt wheel package; the list is not recorded here, so check the spec.
- Checked against 4.7.1 on openSUSE_Tumbleweed, x86_64.

## Workarounds

- Use the upstream wheel for the build architecture instead of the sdist, because the sdist does not contain this distribution's source. This is the binary-only shape the packaging conventions allow for prebuilt extension modules.
- The upstream project's issue about this packaging was not verified; the decision rests on the PyPI release metadata alone (wheels only, no license declared).
- Checked against 4.7.1 on openSUSE_Tumbleweed, x86_64.

## Failure history

- Spec carried TODO/FIXME placeholders (license, runtime Requires, %files). Fixed by filling in the license, the runtime Requires and the exact %files list for the prebuilt wheel package. Checked against 4.7.1 on openSUSE_Tumbleweed, x86_64.
