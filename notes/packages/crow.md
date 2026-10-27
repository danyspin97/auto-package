# crow

## Build requirements
- asio-devel: crow is a header-only C++ library whose devel subpackage ships the headers and CMake config. It needs the standalone asio headers for the asio dependency. The journal does not record whether the spec uses it as BuildRequires or Requires.
- This entry comes from the pre-build source review (medium confidence). No build log in the journal confirms it.
Checked against 1.3.4 on openSUSE_Factory, x86_64.

## Test changes
- Examples and tests are disabled in the CMake configuration. The journal records this as the pre-build plan; it does not confirm the flags were set in the spec.
Checked against 1.3.4 on openSUSE_Factory, x86_64.
