# python-aiocache notes

## Build requirements
- Test dependencies and the optional backend client libraries are needed in %check so the backend modules import during the test run.
Checked against 0.12.3 on openSUSE_Tumbleweed, x86_64.

## Test changes
- Upstream pytest addopts are cleared in %check.
- Tests that need a live cache server are skipped, since no server runs during the build.
- Two decorator tests are deselected because the source does not satisfy them. Their names are not recorded.
Checked against 0.12.3 on openSUSE_Tumbleweed, x86_64.

## Workarounds
- The redis backend calls the async close method of the redis client instead of the older close call.
Checked against 0.12.3 on openSUSE_Tumbleweed, x86_64.

## Failure history
- Redis backend close call failed; fixed by switching to the async close method (attempt 1).
- Two decorator tests failed against the source; fixed by deselecting them (attempt 1).
- Journal entries give the reasoning only, not the error output, so these fixes are the build agent's stated changes.
Checked against 0.12.3 on openSUSE_Tumbleweed, x86_64.
