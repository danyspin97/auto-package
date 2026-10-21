# python-openpyxl notes

## Build requirements
- Requires `et_xmlfile`. That is the name upstream uses for its only runtime dependency (install_requires and Requires-Dist), so the spec must use that exact spelling.
- No compiled extension is built by default. The Cython extension is only built when the Cython option is given, so the default build needs no C toolchain or Cython.
- Upstream sets Requires-Python to 3.8 or newer.

Checked against 3.1.5 on openSUSE_Tumbleweed, x86_64.
