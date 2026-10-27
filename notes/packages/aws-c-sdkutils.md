# aws-c-sdkutils

## Build requirements
- BuildRequires aws-c-common-devel: configure needs its AwsGetVersion.cmake CMake module, which this library does not ship itself.
Checked against 1.0.1 on openSUSE_Factory, x86_64.

## Workarounds
- On openSUSE_Factory, x86_64 only, the installed aws-c-common-devel does not put AwsGetVersion.cmake where the upstream search in source/CMakeLists.txt looks. The spec passes the installed module to CMake by its full path, and limits the lookup to the library directory's cmake tree, because a search of all of /usr stops %build on unreadable directories.
Checked against 1.0.1 on openSUSE_Factory, x86_64.

## Failure history
- Configure failed with "Could not find AwsGetVersion.cmake" from an installed aws-c-common. Fixed by passing the module's full path (see Workarounds), then narrowing the search after a permission-denied error aborted %build.
- The versioned library was unpackaged in check-files, and the spec made a stale .so.1 symlink by hand. Fixed by packaging the SONAME the build installs (.so.1.0 for 1.0.x) and dropping the manual symlink.
- rpmlint error shlib-policy-name-error: the subpackage name must match the SONAME suffix (1_0). Fixed by naming the runtime subpackage libaws-c-sdkutils1_0. (Last checked against 1.0.0.)
- rpmlint warning shared-dir-glob-in-files on the include directory glob. Fixed by listing the headers explicitly under the aws/sdkutils include directory. (Last checked against 1.0.0.)
Checked against 1.0.1 on openSUSE_Factory, x86_64, except where marked.
