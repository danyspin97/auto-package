# python-fsspec notes

## Build requirements
- Build backend is hatchling (>= 1.27.0) with hatch-vcs. hatch-vcs gets its version from git metadata, which the sdist does not contain (see Workarounds).
- Upstream metadata declares no runtime dependencies (no project dependencies in pyproject.toml, no install_requires in setup.cfg), so do not add Requires from upstream metadata.
- Upstream declares no console scripts or entry points.
Checked against 2026.9.0 on openSUSE_Tumbleweed, x86_64.

## Test changes
- The sdist's hatch build config excludes tests directories except the abstract test helpers, so the upstream test suite cannot run from the sdist. The %pytest check is replaced by an import smoke test.
Checked against 2026.9.0 on openSUSE_Tumbleweed, x86_64.

## Workarounds
- The sdist has no git metadata, so hatch-vcs needs a version supplied at build time through the setuptools-scm pretend-version variable, set to 2026.9.0.
Checked against 2026.9.0 on openSUSE_Tumbleweed, x86_64.
