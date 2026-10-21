# python-h11 build notes

**Build requirements**
- Upstream has no build-system table and no install-time dependencies, so no Requires beyond the Python runtime. The build goes through setup.py, so the spec must provide the setuptools tooling it needs.
- Upstream sets python_requires to 3.8 or newer; the Python used for the build must satisfy that.

Checked against 0.16.0 on openSUSE_Tumbleweed, x86_64.

**Test changes**
- %pytest is limited to h11/tests, the test package that setup.py excludes from the installed package. The rest of the tree is not run as tests.

Checked against 0.16.0 on openSUSE_Tumbleweed, x86_64.
