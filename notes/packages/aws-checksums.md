# aws-checksums notes

## Build requirements
- aws-c-common-devel is needed at build time. It provides AwsGetVersion.cmake, which the project's CMake searches for before project() runs.
- On openSUSE_Factory only, the packaged aws-c-common 1.0.2 and aws-c-common-devel 1.0.3-79.1 do not ship that module, so the prefix workaround below is required there.
Checked against 1.0.1 on openSUSE_Factory, x86_64.

## Workarounds
- The aws-c-common install prefix is passed to CMake through the prefix-path variable at configure time. The module search runs before project() sets the system prefixes, so the prefix has to come from the command line.
- The 0.2.11 SOVERSION patch is dropped. It targets the 0.2.11 CMakeLists, and 1.0.1 sets VERSION and SOVERSION from the project version.
- The library subpackage is named for the SOVERSION that CMake installs, and its %files entries must match it. For 1.0.1 that is major.minor (1.0).
Checked against 1.0.1 on openSUSE_Factory, x86_64.

## Failure history
- Configure failed because AwsGetVersion.cmake was not found. Fixed by the prefix-path workaround; waiting for a fixed distro package did not help.
- Install failed because the .so.1 file was missing; only .so.1.0.1, .so.1.0 and .so were installed. Fixed by naming the library subpackage for the 1.0 SONAME and listing the major.minor file in %files.
Checked against 1.0.1 on openSUSE_Factory, x86_64.
