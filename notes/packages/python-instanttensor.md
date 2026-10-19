# python-instanttensor

## Build requirements
- gcc-c++ and make are BuildRequires: the setup script compiles the C++ extension and builds the vendored libaio and liburing itself, so those libraries are not BuildRequires.
- numpy, safetensors and tqdm are BuildRequires only for %check: they come from the package's test extra, not its runtime dependencies.
- torch (>= 2.8.0) is a runtime Requires, from the package's dependency list.
Checked against 0.2.2 on openSUSE_Tumbleweed, x86_64.

## Test changes
- One CUDA-only test is deselected in %check, because it needs a CUDA device the build does not have.
- The upstream tests/cpp directory is copied into tests/ before the tests run, because the PyPI sdist does not ship it.
Checked against 0.2.2 on openSUSE_Tumbleweed, x86_64.

## Workarounds
- The License tag is set to Apache-2.0 by hand; the generated spec carried the full license text and a no-SPDX FIXME.
- Source1, the GitHub tag archive that carries tests/cpp, is fetched by a download_files service, because the package directory otherwise lacks it.
- tests/cpp is copied from the in-tree unpack of Source1: Source1 is unpacked inside the source directory after the build changes into it, so a path one level up does not exist.
Checked against 0.2.2 on openSUSE_Tumbleweed, x86_64.

## Failure history
- Tests needing tests/cpp failed because the directory was missing: added the GitHub tag archive as Source1 and copied tests/cpp from it.
- The CUDA-only test failed in %check: deselected it.
- Source1 was absent from the checkout: added the download_files service.
- The tests/cpp copy failed in %prep with a missing path: copy from the in-tree unpack instead.
Checked against 0.2.2 on openSUSE_Tumbleweed, x86_64.
