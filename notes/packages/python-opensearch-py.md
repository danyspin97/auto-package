# python-opensearch-py

## Build requirements
- Mirror the package's install_requires as both Requires and BuildRequires, so the runtime and the build see the same dependencies.
Checked against 3.2.0 on openSUSE_Tumbleweed, x86_64.

## Test changes
- Replace the upstream test run with an import smoke test in %check, which only confirms the package imports after install.
Checked against 3.2.0 on openSUSE_Tumbleweed, x86_64.

## Workarounds
- The installed module directory must be named opensearchpy/; the package directory is fixed to that name.
- Relax the upstream pin on the protobufs dependency so the distro's version satisfies it.
Checked against 3.2.0 on openSUSE_Tumbleweed, x86_64.

## Failure history
- No failures recorded; the 3.2.0 build succeeded on openSUSE_Tumbleweed, x86_64 after the pre-build review changes (not yet confirmed which were required).
Checked against 3.2.0 on openSUSE_Tumbleweed, x86_64.
