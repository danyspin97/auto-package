# aws-c-event-stream

## Build requirements
- BuildRequires aws-c-common-devel: the upstream configure step loads its AwsGetVersion.cmake module before project(). The distro's aws-c-common-devel (1.0.3 in this build) was sufficient for 1.0.0.
Checked against 1.0.0 on openSUSE_Factory, x86_64.

## Workarounds
- Pass CMAKE_PREFIX_PATH so the pre-project() lookup of AwsGetVersion.cmake finds the system prefix. The upstream lookup hard-codes a lib64 path suffix, so this is only verified where libdir is lib64.
Checked against 1.0.0 on openSUSE_Factory, x86_64.
- No SOVERSION patch is carried. Upstream 1.0.0 derives SOVERSION from its VERSION file, which matches library_soversion 1.
Checked against 1.0.0 on openSUSE_Factory, x86_64.

## Failure history
- Configure failed with AwsGetVersion.cmake not found at CMakeLists.txt line 11, even with aws-c-common-devel installed; fixed by the CMAKE_PREFIX_PATH workaround above.
  Checked against 1.0.0 on openSUSE_Factory, x86_64.
- Installed libraries were .so.1.0.0, .so.1.0 and .so, with SONAME libaws-c-event-stream.so.1.0, but %files listed .so.1; fixed by renaming the library subpackage to libaws-c-event-stream1_0 and matching %files to the SONAME. Upstream 1.0.0 puts the minor version in the SONAME.
  Checked against 1.0.0 on openSUSE_Factory, x86_64.
