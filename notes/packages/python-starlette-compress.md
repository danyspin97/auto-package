# python-starlette-compress

## Build requirements
- Runtime: the upstream project needs starlette and zstandard, and a Brotli backend chosen by interpreter: brotli on CPython, brotlicffi on other interpreters. Require the one that matches the interpreter being built for.
- Upstream restricts Python to versions below 3.14; the package follows that bound.
- Test run: the upstream dev group lists brotlicffi, httpx, trio and uvloop. The pytest run over tests/ needs httpx, trio and uvloop available, so they are test-time requirements.
- Checked against 1.7.1 on openSUSE_Tumbleweed, x86_64.
