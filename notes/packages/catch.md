# catch (Catch2 3.16.1)

## Build requirements
- Upstream sets `cmake_minimum_required` to 3.16, so the build needs CMake 3.16 or newer.
- The -devel package carries the headers and CMake package files; the shared libraries are in runtime subpackages.
Checked against 3.16.1 on openSUSE_Factory, x86_64.

## Test changes
- The test suite runs in %check through ctest; all 82 tests passed.
- The test targets exist only when the upstream development build option is on, and it is off by default.
- Compiler warnings from the upstream test sources (MatchersConstexpr) are left alone; no spec change addresses them.
Checked against 3.16.1 on openSUSE_Factory, x86_64.

## Workarounds
- The source tarball unpacks to `Catch2-3.16.1`, not `Catch-3.16.1`, so the setup step must name that directory.
Checked against 3.16.1 on openSUSE_Factory, x86_64.

## Failure history
- rpmlint shlib-policy-name-error for libCatch2.so.3.16.1 and libCatch2Main.so.3.16.1 (expected suffix 3_16_1). Fixed by naming the runtime subpackage libcatch2-3_16_1.
Checked against 3.16.1 on openSUSE_Factory, x86_64.
