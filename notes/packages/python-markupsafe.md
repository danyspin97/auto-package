# python-markupsafe notes

## Build requirements
- Upstream uses setuptools as its build backend and requires setuptools 77 or newer.
- Upstream has no runtime dependencies beyond Python itself.
- The optional C extension `markupsafe._speedups` makes the installed package architecture-specific, so its files belong in the architecture-specific Python site directory.
- If the C extension fails to compile, upstream's setup script falls back to a pure-Python install instead of failing. The package then builds without the speedups, so a compile failure will not stop the build by itself.
Checked against 3.0.4 on openSUSE_Tumbleweed, x86_64.
