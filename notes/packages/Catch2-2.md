# Catch2-2 notes

## Workarounds
- Needed, not yet applied: the install step wrote no catch2.pc file. The attempt planned to add it, package the shared libraries with ldconfig, and drop an old 2.x-only patch. None of these is in the spec on disk, so they are unverified.
- Needed, not yet applied: the %check step ran the original ctest command. The attempt planned to empty it and to disable the example build. Neither change is in the spec on disk.
- On x86_64 only: the shared libraries were installed to usr/lib64 and no catch2.pc was written.

Checked against 3.16.1 on openSUSE_Factory, x86_64.
