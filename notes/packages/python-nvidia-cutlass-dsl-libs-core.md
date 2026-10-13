# python-nvidia-cutlass-dsl-libs-core

Package for NVIDIA CUTLASS DSL libs core, version 4.7.1.

**Status:** Last build reported as successful. The attempt journal only records attempt 1, which did not build: it fixed the spec parse and left the source-build recipe in place. The journal does not record the build that succeeded, so the requirements and workarounds for it are not documented here yet.

## Failure history

- Attempt 1: `rpmspec --parse` failed on the empty `License:` tag. Fixed by setting it to `LicenseRef-NvidiaProprietary`, the SPDX id used in the NVIDIA source headers.
- Attempt 1: the spec was still the source-build recipe, which the earlier review asked to replace with an upstream-wheel skeleton. Expected to fail in `%build`/`%check`; the journal does not say whether the switch was made.
