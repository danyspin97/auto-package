# influxdb-cxx

## Build requirements
- boost-devel: CMake calls find_package(Boost) and needs the Boost::boost target, so the headers package is required, not only the system library.
- cmake(cpr): the main CMake build calls find_package(cpr REQUIRED).
- cmake(trompeloeil): the test build uses its target.
- The development package of each Boost component that cpr's CMake config looks up must be installed too; the filesystem component is one of them. Without it, configure fails.
Checked against 0.8.1 on openSUSE_Factory, x86_64.

## Test changes
- %check runs ctest directly against the build directory and excludes BoostSupportTest. The reason for the exclusion is not recorded; confirm it before keeping it.
Checked against 0.8.1 on openSUSE_Factory, x86_64.

## Workarounds
- Four older patches were dropped: the 0.8.1 source already contains their changes, or they no longer apply.
- Warnings-as-errors is turned off in the CMake configure, so compiler-version warnings do not fail the build.
- Catch2's version macros header is included before trompeloeil's Catch2 adapter, through CXXFLAGS in %build, because the adapter rejects the include order otherwise. The build got past the mock compile afterwards, but the journal does not confirm this was the only cause.
Checked against 0.8.1 on openSUSE_Factory, x86_64.

## Failure history
- Build rejected at dependency resolution with an empty log while cmake(cpr) and cmake(trompeloeil) were not yet available: named them as BuildRequires; they now resolve.
- CMake configure failed on the Boost components pulled in by cpr: added the filesystem component's development package.
- Test mock compile failed because Catch2's header was included after trompeloeil's adapter: pre-included the version macros header (see Workarounds).
- All 9 tests failed with "Unrecognised token: -E" because the deselect flag after the separator in the %ctest form was passed to the Catch2 binaries: %check now calls ctest directly with the build directory and the deselect flag.
- RPM rejected the spec when the deselect flag followed the separator in the %ctest form ("Unknown option E"): do not use that form.
Checked against 0.8.1 on openSUSE_Factory, x86_64.
