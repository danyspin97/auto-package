# python-brotlicffi

## Build requirements
- cffi is needed at runtime: the package's cffi bindings import it when the module loads.
- hypothesis is test-only; the test suite imports it.
- The package has a compiled cffi extension, so %check and %fdupes use the arch-dependent macro forms, not the noarch ones.
Checked against 1.2.0.1 on openSUSE_Tumbleweed, x86_64.

## Test changes
- Eight tests are deselected. They need uncompressed fixtures (for example zerosukkanooa and alice29.txt) that the upstream libbrotli 1.2.0 test-fixture archive does not contain.
Checked against 1.2.0.1 on openSUSE_Tumbleweed, x86_64.

## Workarounds
- The libbrotli test fixtures are not in the package sources. The spec names the fixture archive as an extra source, the download service fetches it from the spec's source URL, and it is extracted into the bundled libbrotli testdata directory so conftest.py can import.
- The bundled libbrotli is MIT-licensed; its licence text is shipped next to the package licence. This was not required by a failure.
Checked against 1.2.0.1 on openSUSE_Tumbleweed, x86_64.

## Failure history
- Build failed with missing source files for the fixture archive. Fixed by having the download service fetch the archive from the spec's source URL.
- Test collection failed because the libbrotli testdata directory was empty. Fixed by extracting the fixture archive into that directory.
- Eight tests failed because their uncompressed fixtures are absent from the upstream archive. Fixed by deselecting them.
Checked against 1.2.0.1 on openSUSE_Tumbleweed, x86_64.
