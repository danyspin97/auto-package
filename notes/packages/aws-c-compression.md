# aws-c-compression notes

## Build requirements
- cmake(aws-c-common) BuildRequires: required. Upstream CMake calls find_package for aws-c-common with REQUIRED. The version module it includes (AwsGetVersion, which also sets the SOVERSION) comes from the aws-c-common devel package.
- No aws-checksums BuildRequires: upstream 1.0.0 CMake does not reference it, and the build succeeds without it.
Checked against 1.0.0 on openSUSE_Factory, x86_64.

## Workarounds
- The aws-c-common devel package's cmake modules directory must be on the CMake prefix path. Upstream's lookup for AwsGetVersion does not find it otherwise, and configure fails.
Checked against 1.0.0 on openSUSE_Factory, x86_64.

## Failure history
- Configure: AwsGetVersion.cmake not found although aws-c-common devel is installed. Fixed by the prefix path workaround above.
- Packaging: the SONAME symlink libaws-c-compression.so.1.0 was installed but unpackaged. Fixed by setting the library soversion to 1_0 (was 1_0_0) to match the SONAME, using the ldconfig scriptlets macro, and widening the runtime file glob to .so.*.
Checked against 1.0.0 on openSUSE_Factory, x86_64.
