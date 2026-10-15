# python-psycopg-binary notes

## Build requirements
- Test-suite dependencies, including anyio, are needed at build time. The pytest plugins and optional imports they provide are loaded during test collection, which fails without them.
Checked against 3.3.4 on openSUSE_Tumbleweed, x86_64.

## Test changes
- Tests for psycopg_pool and the DNS tests are ignored. They cannot run in the build environment. Confidence medium.
Checked against 3.3.4 on openSUSE_Tumbleweed, x86_64.

## Workarounds
- Build the binary package from the psycopg_c tree through copy_to_binary.py, not from the root psycopg project.
- Patch generators.pyx so Cython 3.3 accepts a redeclared `res` variable. Cython 3.3 rejects the redeclaration.
Checked against 3.3.4 on openSUSE_Tumbleweed, x86_64.

## Failure history
- Cython 3.3 rejected the redeclared `res` variable in generators.pyx: fixed by the generators.pyx patch above.
- Test collection failed on missing test-suite modules and plugins: fixed by adding the test-suite dependencies.
- psycopg_pool and DNS tests could not run in the build environment: fixed by ignoring them.
Checked against 3.3.4 on openSUSE_Tumbleweed, x86_64.
