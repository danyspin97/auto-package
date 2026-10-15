# python-brotlicffi

## Build requirements
- cffi is needed at runtime: the package's cffi bindings import it when the module loads.
- cffi is also needed at build time, because the upstream setup script uses it to generate the extension. Its minimum version depends on the Python version: 1.0.0 before Python 3.13, 1.17.0 from Python 3.13 on.
- hypothesis is test-only; the test suite imports it.
- The package builds a compiled cffi extension, so the fdupes step and the test step use the arch-dependent macro forms, not the noarch ones.
Checked against 1.2.0.1 on openSUSE_Tumbleweed, x86_64.

## Test changes
- Eight tests are deselected: six that read the zerosukkanooa fixture and two streaming-compression tests that read alice29.txt. Each one reads an uncompressed fixture (the compressed fixture's name without the .compressed suffix) that the upstream libbrotli 1.2.0 test-fixture archive does not contain.
Checked against 1.2.0.1 on openSUSE_Tumbleweed, x86_64.

## Workarounds
- The libbrotli test fixtures are not in the package sources. A download service fetches the fixture archive named by the spec's extra source, and %prep extracts it into the bundled libbrotli testdata directory so the test configuration can find the fixtures.
Checked against 1.2.0.1 on openSUSE_Tumbleweed, x86_64.

## Failure history
- Build failed because the fixture archive was missing from the checkout. Fixed by adding the download service that fetches it.
- Test collection failed because the libbrotli testdata directory was empty. Fixed by extracting the fixture archive into that directory in %prep.
- Eight tests failed on missing uncompressed fixtures. Fixed by deselecting them.
Checked against 1.2.0.1 on openSUSE_Tumbleweed, x86_64.
