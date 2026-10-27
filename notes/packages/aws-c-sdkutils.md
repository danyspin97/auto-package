# aws-c-sdkutils

## Build requirements
- BuildRequires aws-c-common-devel: configure needs its AwsGetVersion.cmake CMake module, which this library does not ship itself.
Checked against 1.0.0 on openSUSE_Factory, x86_64.

## Workarounds
- On openSUSE_Factory, x86_64, the installed aws-c-common-devel did not put AwsGetVersion.cmake where the upstream search in source/CMakeLists.txt looks, so configure failed. The spec passes the installed module to CMake by its full path.
Checked against 1.0.0 on openSUSE_Factory, x86_64.

## Failure history
- Configure failed with "Could not find AwsGetVersion.cmake" from an installed aws-c-common. Fixed by passing the module's full path (see Workarounds).
- The versioned library was unpackaged in check-files, and the spec made a stale .so.1 symlink by hand. Fixed by packaging the SONAME the build installs and dropping the manual symlink. For 1.0.0 that SONAME is .so.1.0, so %files and the runtime subpackage must follow the SONAME, not a hard-coded major.
- rpmlint error shlib-policy-name-error: the subpackage name must match the SONAME suffix (1_0). Fixed by renaming the runtime subpackage to libaws-c-sdkutils1_0.
- rpmlint warning shared-dir-glob-in-files on the %{_includedir}/* glob. Fixed by listing the headers explicitly under the aws/sdkutils include directory.
Checked against 1.0.0 on openSUSE_Factory, x86_64.
