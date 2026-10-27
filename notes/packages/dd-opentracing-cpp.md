# dd-opentracing-cpp notes

## Build requirements
- libcurl-devel: the CMake files call find_package(CURL), so it is needed at build time.
- msgpack-cxx-devel (BuildRequires and devel Requires): the public headers include msgpack.hpp, the C++ msgpack API, which this header-only package provides. Consumers that include the headers need it too.
- opentracing-cpp-devel (BuildRequires and devel Requires): the public headers include the OpenTracing C++ API. Consumers need it for the same reason.
- The unversioned msgpack-devel and opentracing-devel names are not published on openSUSE_Factory, x86_64 only; use the -cxx and -cpp names above.
Checked against 1.3.7 on openSUSE_Factory, x86_64.

## Test changes
- The upstream project has no test suite: CMakeLists.txt has no enable_testing() and BUILD_TESTING defaults to OFF. The spec has a note in place of %check and no tests are run.
Checked against 1.3.7 on openSUSE_Factory, x86_64.

## Workarounds
- Source0 is the shipped dd-opentracing-cpp-1.3.7.tar.gz in the package directory; nothing is fetched at build time.
- The _service file still pins upstream tag v1.1.2 with the tar_scm mode disabled. It does not drive the build and was left unchanged.
- A stale dd-opentracing-cpp-1.1.2.tar.xz is still in the package files and is not used.
Checked against 1.3.7 on openSUSE_Factory, x86_64.

## Failure history
- Source0 not found: the spec Version was 1.1.2 while only the 1.3.7 tarball ships. Fixed by setting Version to 1.3.7. Version was reverted to 1.1.2 once in a later attempt, so check it first when Source0 is missing.
- BuildRequires/Requires named the unpublished msgpack-devel and opentracing-devel on openSUSE_Factory, x86_64 only. Fixed by using msgpack-cxx-devel and opentracing-cpp-devel.
Checked against 1.3.7 on openSUSE_Factory, x86_64.
