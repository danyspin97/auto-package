# python-starlette-compress

## Build requirements
- Runtime: the upstream project needs starlette and zstandard, plus a Brotli backend chosen by interpreter: python-Brotli on CPython, brotlicffi on other interpreters. Require the one that matches the interpreter being built for. The spec should list these, not only python-starlette.
- Upstream restricts zstandard and Python to versions below 3.14; the package keeps the same bound.
- Test-time: the upstream dev group lists brotlicffi, httpx, trio and uvloop. A grep of tests/ found no uvloop use and trio only in a type annotation, so uvloop and trio are not shown to be needed. Neither has been confirmed by a test run, and the httpx need was not checked.
- Checked against 1.7.1 on openSUSE_Tumbleweed, x86_64.
