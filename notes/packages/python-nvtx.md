# python-nvtx notes

## Build requirements
- The package compiles a Cython extension (cythonize over the .pyx files in the nvtx _lib directory, driven by setup.py), so it is architecture-specific: the duplicate-file cleanup targets the architecture-specific Python site directory, not the noarch one.
- The License field uses the SPDX expression from pyproject.toml, and LICENSE.txt is shipped with the license macro.
Checked against 0.2.15 on openSUSE_Tumbleweed, x86_64.

## Test changes
- The sdist's tests import a conftest module that the sdist does not include, so the test run is replaced with an import check: the nvtx package and nvtx._lib.profiler are imported from the buildroot. No individual tests are deselected.
Checked against 0.2.15 on openSUSE_Tumbleweed, x86_64.
