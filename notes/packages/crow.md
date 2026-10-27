# crow

## Build requirements
- Header-only library with no shared library: the base package carries the license, and the -devel subpackage ships the headers (include/) and the CMake config (lib/cmake/Crow).
- asio-devel: the CMake config looks up the standalone asio dependency (a Findasio module is installed with it), so the asio headers are needed for the build. The notes do not record whether the spec uses BuildRequires or Requires for it.
Checked against 1.3.4 on openSUSE_Factory, x86_64.

## Test changes
- The test suite (CROW_BUILD_TESTS) and the examples are switched off in the spec, with a comment giving the reason. The test build fetches Catch2 from GitHub through CPM at configure time, and OBS builds have no network access, so any build step that downloads during configure will fail there.
- Earlier spec comments claimed the test suite needs Boost, SSL and fixtures. That was wrong; the blocker is the Catch2 download.
Checked against 1.3.4 on openSUSE_Factory, x86_64.
