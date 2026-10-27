# CRCpp notes

## Workarounds

- The old CMake install-directory patch is dropped. Upstream releases that use the CMAKE_INSTALL_INCLUDEDIR variable do not need it, and a patch that targets CMAKE_INSTALL_INCLUDE_DIR no longer matches the source.
- The devel subpackage must own the CPS package-info directory under the shared data directory, because upstream's install(PACKAGE_INFO) step writes its output there.

Checked against 1.2.3.0 on openSUSE_Factory, x86_64.
