# python-nvtx notes

## Build requirements
- The package compiles a Cython extension (setup.py cythonizes the .pyx files in the nvtx _lib directory), so it is architecture-specific: the duplicate-file cleanup (fdupes) targets the architecture-specific Python site directory, not the noarch one.
- The License field uses the SPDX expression from pyproject.toml (Apache-2.0 WITH LLVM-exception), and the LICENSE file is shipped with the license macro.
- BuildRequires setuptools at the minimum version the build-system section of pyproject.toml requires; the build needs at least that version.
- numpy and matplotlib are Recommends, not Requires: they appear only in the optional test and docs extras of pyproject.toml, so the runtime does not need them.
Checked against 0.2.15 on openSUSE_Tumbleweed, x86_64.

## Test changes
- The sdist's tests import a conftest module that the sdist does not include, so the %pytest run cannot work. It is replaced with an import check of the nvtx package and nvtx._lib.profiler from the buildroot. No individual tests are deselected.
Checked against 0.2.15 on openSUSE_Tumbleweed, x86_64.
