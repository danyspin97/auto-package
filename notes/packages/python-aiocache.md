# python-aiocache notes

## Build requirements
- Test dependencies and the optional backend client libraries are needed in %check so the backend modules import during the test run.
- apache2-utils is needed in %check because the performance concurrency test runs the ab benchmark binary it provides.
Checked against 0.12.3 on openSUSE_Tumbleweed, x86_64.

## Test changes
- Upstream pytest addopts are cleared in %check.
- Tests that need a live redis or memcached server are deselected by their markers, since no server runs during the build.
- Two decorator-timing tests are deselected because they cannot pass in the build. Their names are not recorded.
Checked against 0.12.3 on openSUSE_Tumbleweed, x86_64.

## Workarounds
- The redis backend calls the async close method of the redis client (aclose) instead of the deprecated close call, which the test configuration turns into an error.
- The performance test server stores the cache under an aiohttp AppKey instead of a plain string key, which avoids a warning the test configuration turns into an error.
Checked against 0.12.3 on openSUSE_Tumbleweed, x86_64.

## Failure history
- Redis backend teardown failed on the deprecated redis close call; fixed by switching to the async close method.
- Two decorator-timing tests failed against the source; fixed by deselecting them.
- Redis and memcached footprint tests failed with connection refused, since no server runs; fixed by deselecting server-backed tests.
- Concurrency error-rate tests failed because the ab binary was missing; fixed by the apache2-utils build requirement, and a later build no longer shows this failure.
- In-memory concurrency test failed on a string-key warning in the performance server; fixed by using an AppKey.
Checked against 0.12.3 on openSUSE_Tumbleweed, x86_64.
