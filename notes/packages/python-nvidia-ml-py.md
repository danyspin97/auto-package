# python-nvidia-ml-py

## Workarounds
- The upstream sdist is a bare py_modules project: it installs its module file directly into top-level site-packages, with no package directory. %files must list the module file and its __pycache__ entry; a package-directory entry fails with "File not found".
- The sdist also installs an example script at top-level site-packages. The spec removes it in %install, along with its bytecode and its RECORD entry.
- License is BSD-3-Clause, the valid SPDX identifier, not the bare "BSD".
- The changelog entry was written by hand because the changelog tool cannot authenticate in this environment.
Checked against 13.615.71 on openSUSE_Tumbleweed, x86_64. The build succeeded; the changelog entry was not verified by it.

## Test changes
- The sdist ships no test suite, so %check runs an import smoke test of the module instead.
Checked against 13.615.71 on openSUSE_Tumbleweed, x86_64.
