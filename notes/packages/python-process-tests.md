# python-process-tests

**Build requirements**
- Upstream build-system requires setuptools>=30.3.0 and declares no runtime dependencies, so no Requires beyond what the build generates.
- Modules in src/ install at the top level of site-packages (for example process_tests.py), not under a src/ directory; %files must list them there.
Checked against 3.0.0 on openSUSE_Tumbleweed, x86_64.

**Test changes**
- %check runs the test suite by calling the flavor's interpreter with pytest directly. The %pytest macro must not be used inside %{python_expand}.
Checked against 3.0.0 on openSUSE_Tumbleweed, x86_64.

**Workarounds**
- %check sets PYTHONUNBUFFERED, as upstream's test environment does. The tests read a child process's stdout through a pipe, so the child must be unbuffered for its startup output to arrive.
Checked against 3.0.0 on openSUSE_Tumbleweed, x86_64.

**Failure history**
- `bad substitution` in %check when %pytest was used inside %{python_expand}: fixed by running pytest with the flavor interpreter directly.
- test_socket timed out after 60 s with empty captured server output because the child's stdout was buffered: fixed by setting PYTHONUNBUFFERED in %check; the build succeeds with it. The journal records no per-test result after that change.
Checked against 3.0.0 on openSUSE_Tumbleweed, x86_64.
