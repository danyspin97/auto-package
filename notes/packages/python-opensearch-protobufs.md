# python-opensearch-protobufs

## Build requirements
- The protobuf sources are compiled with grpcio-tools instead of Bazel. This was the pre-build plan; the journal does not record a separate check of it.
Checked against 1.2.0 on openSUSE_Tumbleweed, x86_64.

## Workarounds
- pyproject.toml is written out in %prep instead of being shipped as an untracked Source1 file. Reason: the untracked extra source was replaced so the build uses only tracked inputs.
Checked against 1.2.0 on openSUSE_Tumbleweed, x86_64.

## Failure history
- A build failure after the first attempt was traced to the untracked pyproject.toml Source1; fixed by writing the file in %prep. The exact error is not recorded in the journal.
Checked against 1.2.0 on openSUSE_Tumbleweed, x86_64.
