# python-redis notes

## Build requirements
- The upstream package uses the hatchling build backend and ships no compiled extensions, so no compiler toolchain is needed to build it. Checked against 8.0.1 on openSUSE_Tumbleweed, x86_64.
- The upstream metadata requires async-timeout on Python versions below 3.11.3, at version 4.0.3 or newer. Checked against 8.0.1 on openSUSE_Tumbleweed, x86_64, pre-build review only.
- Upstream test dependencies are listed in the dev requirements file: redis-entraid, pybreaker, numpy, packaging, pytest-asyncio, pytest-timeout and pytest-cov. The file does not list mock. Checked against 8.0.1 on openSUSE_Tumbleweed, x86_64, pre-build review only.

## Workarounds
- The pre-build review planned to drop two patches that no longer apply to 8.0.1 and to bump async-timeout to 4.0.3. The build succeeded on this target, but the journal does not record whether these exact changes were all applied. Checked against 8.0.1 on openSUSE_Tumbleweed, x86_64, pre-build review only.
