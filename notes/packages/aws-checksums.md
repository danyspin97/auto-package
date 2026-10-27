# aws-checksums notes

## Build requirements
- aws-c-common-devel is needed at build time. It provides AwsGetVersion.cmake, which the project's CMake searches for before project() runs. On openSUSE_Factory, aws-c-common 1.0.2 and aws-c-common-devel 1.0.3-79.1 do not ship that module.
Checked against 1.0.1 on openSUSE_Factory, x86_64.
- The shared library subpackage name and its %files entries must follow the SOVERSION that CMake actually installs. For 1.0.1 that is major.minor (1.0), not the major alone.
Checked against 1.0.1 on openSUSE_Factory, x86_64.

## Workarounds
- The aws-c-common install prefix is passed through the CMake prefix-path variable at configure time, because the module search runs before project() has set the system prefixes. Used while the distro module was missing.
Checked against 1.0.1 on openSUSE_Factory, x86_64.
- The 0.2.11 SOVERSION patch was removed. CMake for 1.0.1 sets VERSION and SOVERSION from the project version, and the spec's library version follows %{version}.
Checked against 1.0.1 on openSUSE_Factory, x86_64.

## Failure history
- Configure failed: AwsGetVersion.cmake not found. Fixed by the prefix-path workaround above (openSUSE_Factory, aws-c-common-devel 1.0.3-79.1). Earlier waits for a fixed distro package did not help.
- Install failed: the library file for SONAME .so.1 was missing; only .so.1.0.1, .so.1.0 and .so were installed. Fixed by renaming the library subpackage to match the 1.0 SONAME and listing the major.minor file in %files.
