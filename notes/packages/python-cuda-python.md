# python-cuda-python notes

## Build requirements

- The spec builds only the `cuda_python` subdirectory of the upstream monorepo. That subdirectory is a pure-Python setuptools metapackage with no compiled code, so the other components are not built here.
- BuildRequires setuptools, setuptools_scm and packaging, which the build needs at build time.
- Requires lists the runtime sibling components pinned in the upstream setup file, because the metapackage has no code of its own and only pulls them in.
- The sibling names were verified against PyPI, not against distro package names. When a build fails on a missing requirement, confirm the names against the build target.

Checked against 13.4.4 on openSUSE_Tumbleweed, x86_64.

## Test changes

- The %check section runs a metadata smoke test that the spec adds. It passed in the 13.4.4 build.

Checked against 13.4.4 on openSUSE_Tumbleweed, x86_64.

## Workarounds

- The generator produced a monorepo skeleton with TODO placeholders and no real build steps. The single-subproject spec was written by hand in this attempt. The generator should produce this scoped spec itself, so the spec does not need to be edited again.
- When the build service client cannot reach its credential keyring from the build environment, the changelog entry must be written by hand.

Checked against 13.4.4 on openSUSE_Tumbleweed, x86_64.
