# flatbuffers

## Test changes

- The test suite runs through CTest in the check step and passes. Nothing is deselected or patched.

Checked against 25.12.19-2026-02-06-03fffb2 on openSUSE_Factory, x86_64.

## Workarounds

- The upstream source archive's top-level directory is named after the full release string, not after `flatbuffers-<version>`. The unpack step must point at that real directory.
- The upstream release string is not a legal RPM Version. Keep Version as the legal numeric part and carry the full upstream string in a separate macro, used for the source tarball name and the unpack directory.
- The static library target is turned off at configure time, and no static archive is packaged in -devel. Packaging it fails rpmlint. flatc and the tests do not link that target, so nothing else needs it.

Checked against 25.12.19-2026-02-06-03fffb2 on openSUSE_Factory, x86_64.

## Failure history

- The static flatbuffers archive in -devel failed rpmlint (static-library-without-debuginfo and lto-no-text-in-archive, badness 10000 over the 999 threshold). Fixed by disabling the static library at configure time and removing it from the devel file list.

Checked against 25.12.19-2026-02-06-03fffb2 on openSUSE_Factory, x86_64.
