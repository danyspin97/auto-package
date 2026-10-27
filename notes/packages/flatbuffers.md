# flatbuffers

## Workarounds

- The upstream source archive's top-level directory is named after the full release string, not after `flatbuffers-<version>`. The unpack step must point at that real directory.
- The upstream release string is not a legal RPM Version. Keep Version as the legal numeric part and carry the full upstream string in a separate macro, used for the source tarball name and the unpack directory.

Checked against 25.12.19-2026-02-06-03fffb2 on openSUSE_Factory, x86_64.
