# python-opensearch-py

## Build requirements
- Mirror the package's install_requires as both Requires and BuildRequires, so the runtime and the build see the same dependencies. urllib3 must also be a BuildRequires because the %check import loads the package, which imports urllib3.
Checked against 3.2.0 on openSUSE_Tumbleweed, x86_64.

## Test changes
- The sdist ships no tests, so %check is an import smoke test: it only confirms the top-level module imports after install.
Checked against 3.2.0 on openSUSE_Tumbleweed, x86_64.

## Workarounds
- The installed module directory must be named opensearchpy/, the top-level name the package declares.
- Relax the upstream pin on the protobufs dependency so the distro's version satisfies it.
Checked against 3.2.0 on openSUSE_Tumbleweed, x86_64.

## Failure history
- No failures recorded for 3.2.0: the first build after the pre-build review succeeded. Which review changes were required is not confirmed.
Checked against 3.2.0 on openSUSE_Tumbleweed, x86_64.
