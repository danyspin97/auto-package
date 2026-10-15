# python-opensearch-protobufs

## Build requirements
- The protobuf sources are compiled to Python with grpcio-tools in %build instead of Bazel, which the upstream archive ships a BUILD.bazel for. The build interpreter (python3) must be able to import grpcio-tools.
Checked against 1.2.0 on openSUSE_Tumbleweed, x86_64.

## Workarounds
- The upstream archive has no pyproject.toml, setup.py or setup.cfg at any depth, so the packaging metadata is written out in %prep.
Checked against 1.2.0 on openSUSE_Tumbleweed, x86_64.

## Failure history
- The protoc call in %build used the bare python variable, which is only defined inside %python_expand, so the call failed with a command-not-found error. Fixed by calling python3 in %build.
Checked against 1.2.0 on openSUSE_Tumbleweed, x86_64.
