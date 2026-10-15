## Workarounds

Checked against 1.3.1 on openSUSE_Tumbleweed, x86_64.

- The upstream metadata caps Requires-Python below 3.11, and the upstream README says the package should not be used on 3.11 and newer. The pip installs in %build and %install ignore the Requires-Python cap, so every Python flavor builds and installs.

## Failure history

Checked against 1.3.1 on openSUSE_Tumbleweed, x86_64.

- pip in %build refused every flavor for 3.11 and newer because of the Requires-Python cap: fixed by ignoring the cap in %build.
- pip install in %install hit the same refusal for 3.11 and newer flavors: fixed by ignoring the cap in %install as well.
