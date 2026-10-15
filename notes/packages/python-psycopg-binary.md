# python-psycopg-binary notes

## Build requirements
- The test-suite dependencies anyio, dnspython, mypy and pproxy are needed at build time. Pytest plugins and the modules that DNS and proxy tests import are loaded during collection, which fails without them.
- Cython and the libpq development files are needed: the C extension is built with Cython and links against libpq. Cython 3.3 rejects the source as shipped, so the source patch below is required.
- The psycopg_c tree declares no runtime dependencies, so the binary package adds none to Requires.
Checked against 3.3.4 on openSUSE_Tumbleweed, x86_64.

## Test changes
- psycopg_pool tests are ignored. They cannot run in the build environment.
- DNS SRV tests (tests/test_dns_srv.py) are deselected. Their fixtures read the resolver configuration file, which the build root does not provide, so they error.
- test_resolve_hostaddr_async_warning is deselected for the same resolver reason.
- test_gssencmode_default is deselected. It expects libpq's default gssencmode to be 'disable', but libpq defaults to 'prefer'.
Checked against 3.3.4 on openSUSE_Tumbleweed, x86_64.

## Workarounds
- Build the binary package from the psycopg_c tree through copy_to_binary.py, not from the root project. The root pyproject has no project table and cannot produce the binary package.
- Patch generators.pyx so Cython 3.3 accepts the redeclared `res` variable. This is a source-level fix, not a build flag.
Checked against 3.3.4 on openSUSE_Tumbleweed, x86_64.

## Failure history
- Cython 3.3 rejected the redeclared `res` in generators.pyx: fixed by the generators.pyx patch.
- Test collection failed on missing test-suite modules and plugins: fixed by adding the test-suite dependencies.
- psycopg_pool and DNS tests could not run in the build environment: fixed by ignoring them.
- DNS SRV and resolver-dependent tests errored without a resolver configuration file: fixed by deselecting them.
- test_gssencmode_default failed because libpq's default is 'prefer': fixed by deselecting it.
Checked against 3.3.4 on openSUSE_Tumbleweed, x86_64.
