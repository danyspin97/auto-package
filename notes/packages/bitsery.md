# bitsery notes

## Test changes
- The test CMake file hard-codes -Werror on the test targets. It is removed from the test flags so that GCC warnings from the headers (for example unused-but-set variables) do not fail the test build.
- Checked against 5.2.5 on openSUSE_Factory, x86_64.

## Workarounds
- The pointer utilities header is patched to drop redundant parentheses from the void-pointer reference parameter declarations. Reason: GCC -Wparentheses fails the test build under -Werror.
- Checked against 5.2.5 on openSUSE_Factory, x86_64.

## Failure history
- Test build failed with -Werror=parentheses on the pointer utilities header (on openSUSE_Factory, x86_64 only) → removed the redundant parentheses.
- Test build failed with -Werror=unused-but-set-variable in the deserialized type id helper of the pointer utilities header (on openSUSE_Factory, x86_64 only) → removed -Werror from the test flags; the header is unchanged.
- Checked against 5.2.5 on openSUSE_Factory, x86_64.
