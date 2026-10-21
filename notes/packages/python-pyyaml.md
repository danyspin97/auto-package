# python-pyyaml notes

## Build requirements
- BuildRequires libyaml development files. PyYAML's optional C extension links against libyaml, so the headers must be present at build time.
- The upstream build backend is a local PEP 517 backend that requires setuptools and Cython at build time, so both must be available to the build.
Checked against 6.0.3 on openSUSE_Tumbleweed, x86_64.

## Workarounds
- The spec's file list and install tree were corrected before the first build, so the packaged files match what the build installs.
- The spec's test macro was changed before the first build. The journal does not record the change, so check the spec before relying on it.
Checked against 6.0.3 on openSUSE_Tumbleweed, x86_64. Source: pre-build review, medium confidence; the build then succeeded.
