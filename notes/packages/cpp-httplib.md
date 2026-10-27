# cpp-httplib

## Build requirements
- zstd support is optional upstream: HTTPLIB_USE_ZSTD_IF_AVAILABLE defaults ON and HTTPLIB_REQUIRE_ZSTD defaults OFF, so zstd is used only when its development files are present at build time.
- The shared library's SOVERSION is MAJOR.MINOR, so the soname tracks the minor version. Any file or subpackage name that carries the version changes with each minor release.
- The source tarball unpacks to cpp-httplib-<version>, which is the default setup directory, so no custom setup directory is needed.

Checked against 0.63.0 on openSUSE_Factory, x86_64.
