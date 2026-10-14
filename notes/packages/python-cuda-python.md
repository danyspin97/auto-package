# python-cuda-python notes

## Build requirements

- The spec builds only the `cuda_python` subdirectory of the upstream monorepo. That subdirectory is a pure-Python setuptools metapackage with no compiled code, so the other components are not built here.
- Requires lists the runtime sibling components, because the metapackage has no code of its own and only pulls them in.
- The sibling names were verified against PyPI at review time. That check does not show that the distribution packages of the same names exist, so confirm them against the build target when a build fails on a missing requirement.

Checked against 13.4.4 on openSUSE_Tumbleweed, x86_64.

## Workarounds

- The generator produced a monorepo skeleton with TODO placeholders and no real build steps. The spec was rewritten by hand to build only the metapackage. The generator should be fixed to produce this scoped spec, rather than the spec being edited again.
- When the OBS command-line client cannot reach the server from the build environment, the changelog entry must be written by hand. Seen on home:dspinella:auto-package only.

Checked against 13.4.4 on openSUSE_Tumbleweed, x86_64.
