# python-coherent.licensed notes

## Build requirements
- flit-core is BuildRequires once. The upstream build backend is flit_core.buildapi, so flit-core is the only build-time dependency.
- Do not pin an upper bound on flit-core. Distros ship a newer flit-core than the bound upstream allows, and the bound breaks the build.
- Upstream declares no runtime dependencies, so the package needs no Requires beyond Python. Upstream requires Python >= 3.9.
Checked against 0.5.2 on openSUSE_Tumbleweed, x86_64.

## Test changes
- The sdist ships no test suite (no tests directory), so %check is an import smoke test of the coherent.licensed modules.
- The coherent/ namespace directory has no __init__.py, so the smoke test imports the modules by their full dotted names.
Checked against 0.5.2 on openSUSE_Tumbleweed, x86_64.
