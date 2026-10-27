# aws-c-io

## Build requirements
- The package must be upstream 1.1.0 or newer: that release installs aws/io/l4_proxy.h, which 0.27.7 lacks.
- The aws-c-common development package must be installed, because the CMake file finds it and its CMake modules are looked up at configure time.
Checked against 1.1.1 on openSUSE_Factory, x86_64 (the l4_proxy item was last checked against 1.1.0).

## Workarounds
- The soname stays 0unstable across version updates: the spec carries no SOVERSION patch, and the prep stage pins the soname instead.
- The install prefix is passed to CMake's prefix search path so aws-c-common's AwsGetVersion.cmake is found.
Checked against 1.1.1 on openSUSE_Factory, x86_64.

## Failure history
- Configure failed with "Could not find AwsGetVersion.cmake from an installed aws-c-common": fixed by passing the install prefix to CMake.
Checked against 1.1.1 on openSUSE_Factory, x86_64.
