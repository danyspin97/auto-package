# python-redis notes

## Build requirements
- The upstream package uses the hatchling build backend and ships no compiled extensions, so no compiler toolchain is needed. Checked against 8.0.1 on openSUSE_Tumbleweed, x86_64.
- The upstream metadata requires async-timeout on Python versions below 3.11.3, at version 4.0.3 or newer. The Requires must use the distro package name async_timeout rather than the PyPI spelling. Checked against 8.0.1 on openSUSE_Tumbleweed, x86_64, pre-build review.
- The observability tests import opentelemetry, so the spec needs an opentelemetry BuildRequires for the test run. Checked against 8.0.1 on openSUSE_Tumbleweed, x86_64, pre-build review.
- Upstream test dependencies are listed in the dev requirements file: redis-entraid, pybreaker, numpy, packaging, pytest-asyncio, pytest-timeout and pytest-cov. The file does not list mock. Checked against 8.0.1 on openSUSE_Tumbleweed, x86_64, pre-build review.

## Workarounds
- Two patches carried for earlier versions were removed because they no longer apply to 8.0.1. The build succeeded on this target without them. Checked against 8.0.1 on openSUSE_Tumbleweed, x86_64, pre-build review; the journal records only the plan, not each applied change.
