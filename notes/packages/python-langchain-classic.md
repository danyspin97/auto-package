# python-langchain-classic

## Build requirements
- langchain-openai is a test-only BuildRequires. The unit tests import it, and FlareChain refuses to run without it, although the runtime does not need it.
- async-timeout is required only on Python older than 3.11. The runtime imports it only there, so the Requires must be conditional on the Python version. Wrap the condition in parentheses; without them the build service reads the `if` as a package name.
- The SQLAlchemy Requires must use the spelling of the upstream metadata.
- pytest-socket is needed for the upstream-style test run that blocks network sockets.
Checked against 1.0.7 on openSUSE_Tumbleweed, x86_64.

## Test changes
- Only tests/unit_tests are run.
- test_socket_disabled is deselected. On an offline build worker, name resolution fails before pytest-socket sees the connection, so the test gets a connection error instead of the blocked-socket error. This is the worker's network state, not a code problem.
- test_configurable is deselected. Its prompt_cache_options snapshot differs with the langchain-openai in the build repository. This is version skew, not a regression in this code.
Checked against 1.0.7 on openSUSE_Tumbleweed, x86_64.

## Failure history
- test_flare failed with an ImportError asking for langchain-openai. Fixed by adding langchain-openai as a test BuildRequires.
- test_socket_disabled got a connection error on the offline worker. Fixed by deselecting it.
- test_configurable snapshot mismatch against the newer langchain-openai. Fixed by deselecting it.
- The conditional async-timeout Requires was read as a package name. Fixed by parenthesizing the condition.
Checked against 1.0.7 on openSUSE_Tumbleweed, x86_64.
