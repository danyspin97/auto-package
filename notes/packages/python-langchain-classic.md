# python-langchain-classic

## Build requirements
- langchain-openai is a test-only BuildRequires. The unit tests import it, and FlareChain refuses to run without it, although the runtime does not need it.
- async-timeout is required only on Python older than 3.11. The runtime imports it only there, so the Requires must be conditional on the Python version.
- The SQLAlchemy Requires must use the spelling of the upstream metadata.
- pytest-socket is needed for the upstream-style test run that blocks network sockets.
Checked against 1.0.7 on openSUSE_Tumbleweed, x86_64.

## Test changes
- Only tests/unit_tests are run.
- test_socket_disabled is deselected. The build worker is offline, so name resolution fails before pytest-socket sees the connection, and the test gets a connection error instead of the blocked-socket error. This is the worker's network state, not a code problem.
- test_configurable is deselected. Its prompt_cache_options snapshot differs with the newer langchain-openai in the build repository, which is version skew rather than a regression in this code.
Checked against 1.0.7 on openSUSE_Tumbleweed, x86_64.

## Failure history
- test_flare failed with an ImportError asking for langchain-openai. Fixed by adding langchain-openai as a test BuildRequires.
- test_socket_disabled got a connection error on the offline worker. Fixed by deselecting it.
- test_configurable snapshot mismatch against the newer langchain-openai. Fixed by deselecting it.
Checked against 1.0.7 on openSUSE_Tumbleweed, x86_64.
