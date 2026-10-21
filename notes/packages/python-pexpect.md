# python-pexpect

## Build requirements
- BuildRequires man, zsh and openssl-3. The openssl package is needed because the performance and chunk tests run the openssl command; the journal does not record which tests need man or zsh.
- BuildRequires aiounittest for the async tests. Planned in the review; the journal records no build check of it.
- Requires ptyprocess>=0.5, the only install dependency in setup.py.
Checked against 4.9.0 on openSUSE_Tumbleweed, x86_64 (build succeeded; entries taken from the journal, not re-read from the spec).

## Test changes
- PYTHON_BASIC_REPL=1 is set in %check. The journal records no failure it fixes.
Checked against 4.9.0 on openSUSE_Tumbleweed, x86_64 (build succeeded; from the journal, not re-verified).

## Workarounds
- Quilt patch that makes the tests use the fork multiprocessing start method. Under the forkserver start method, the tests pickle TestCase objects and local classes, which fails. Applies wherever the Python default start method is forkserver.
Checked against 4.9.0 on openSUSE_Tumbleweed, x86_64 (build succeeded; from the journal, medium confidence).

## Failure history
- Socket tests failed with "cannot pickle module object" under forkserver. Fixed by the fork start method patch.
- test_expect test_stdin_closed failed with a PicklingError on a local object under forkserver. Fixed by the fork start method patch.
- test_performance and test_before_across_chunks failed (assert 0 == 500) because openssl was not found. Fixed by BuildRequires openssl-3.
Checked against 4.9.0 on openSUSE_Tumbleweed, x86_64.
