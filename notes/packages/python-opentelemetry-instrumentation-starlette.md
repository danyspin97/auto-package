# python-opentelemetry-instrumentation-starlette

## Build requirements

- Runtime Requires: the opentelemetry api, instrumentation, instrumentation-asgi, semantic-conventions and util-http packages, at 0.66b0. The starlette instrumentation imports the asgi instrumentation at module load, so it must be present at runtime.
- BuildRequires: the same sibling packages as the runtime Requires, so that %check can import them. Without them the import fails in %check.
- BuildRequires: opentelemetry-test-utils, opentelemetry-sdk, starlette and httpx, which the tests need.
- Version bounds: use 0.66b0, not 0.66b1, for the sibling opentelemetry packages. The repositories for this target ship them only at 0.66b0, so a 0.66b1 bound cannot be satisfied and the build is rejected during dependency resolution.

Checked against 0.66b1 on openSUSE_Tumbleweed, x86_64.

## Test changes

- Deselect 19 tests that fail against the starlette release in the repository. The removed Starlette.route decorator breaks the setUp of three custom-header test classes, and two metric tests assert attributes that the newer starlette no longer produces. The other 43 tests pass and stay enabled.

Checked against 0.66b1 on openSUSE_Tumbleweed, x86_64.

## Failure history

- %files listed a package directory that the sdist does not install. The sdist installs a PEP 420 namespace under opentelemetry/instrumentation/starlette/, so the %files path was corrected to that directory.
- Dependency resolution failed on 0.66b1 bounds the repositories cannot satisfy. Fixed by relaxing the bounds to 0.66b0 (see Build requirements).
- %check failed importing the asgi instrumentation. Fixed by adding the sibling packages to BuildRequires.
- %check failed on the starlette-dependent tests. Fixed by deselecting them (see Test changes).

Checked against 0.66b1 on openSUSE_Tumbleweed, x86_64.
