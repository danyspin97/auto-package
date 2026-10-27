# aws-c-common notes

## Build requirements
- Upstream 1.0.3 installs its CMake helper modules, including AwsGetVersion.cmake, into the package's CMake config directory under the CMake libdir. The installed config file adds that directory to consumers' module path. The devel file list must own that directory. Keep that install step; no patch is needed for it.
  Checked against 1.0.3 on openSUSE_Factory, x86_64 (successful build with the install step unchanged; the installed files were not inspected).

## Failure history
- The 1.0.3 build on openSUSE_Factory, x86_64 finished with no failures and needed no patches or test changes.
  Checked against 1.0.3 on openSUSE_Factory, x86_64.
