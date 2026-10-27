# cmis-client notes

## Build requirements
- `xmlto`: configure.ac requires it to generate the man page, so it must be a BuildRequires.

Checked against 0.6.3 on openSUSE_Factory, x86_64.

## Workarounds
- configure.ac: the sed that rewrites the system include directory to `-isystem` is removed. Passing the system include directory as `-isystem` breaks the C++ standard library's wrapper for `stdlib.h` (its include-next lookup fails), so the build fails. Keep system include directories out of `-isystem`.
- The four patches for the SHA-1 digest code in the XML utilities are no longer applied. 0.6.3 carries the fixes upstream, and the build succeeds without them.

Checked against 0.6.3 on openSUSE_Factory, x86_64.

## Failure history
- Compile of the atom folder failed with "stdlib.h: No such file or directory" because the system include directory was passed as `-isystem`. Fixed by removing that rewrite from configure.ac (see Workarounds).

Checked against 0.6.3 on openSUSE_Factory, x86_64.
