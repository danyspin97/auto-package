# aws-checksums notes

## Build requirements
- aws-c-common-devel is needed at build time. It provides AwsGetVersion.cmake, which the project's CMake searches for before project() runs.
- On openSUSE_Factory only, aws-c-common 1.0.2 and aws-c-common-devel 1.0.3-79.1 do not ship that module.
Checked against 1.0.1 on openSUSE_Factory, x86_64.
- The shared library subpackage name and its %files entries must follow the SOVERSION that CMake installs. For 1.0.1 that is major.minor (1.0), not the major alone.
Checked against 1.0.1 on openSUSE_Factory, x86_64.

## Workarounds
- The aws-c-common install prefix is passed through the CMake prefix-path variable at configure time. The module search runs before project() sets the system prefixes, so the prefix has to come from the command line.
Checked against 1.0.1 on openSUSE_Factory, x86_64.
- The 0.2.11 SOVERSION patch was dropped. It targets the 0.2.11 CMakeLists, and 1.0.1 sets VERSION and SOVERSION from the project version.
Checked against 1.0.1 on openSUSE_Factory, x86_64.

## Failure history
- Configure failed because AwsGetVersion.cmake was not found. Fixed by the prefix-path workaround. Waiting for a fixed distro package did not help.
Checked against 1.0.1 on openSUSE_Factory, x86_64.
- Install failed because the .so.1 file was missing; only .so.1.0.1, .so.1.0 and .so were installed. Fixed by naming the library subpackage for the 1.0 SONAME and listing the major.minor file in %files.
Checked against 1.0.1 on openSUSE_Factory, x86_64.
