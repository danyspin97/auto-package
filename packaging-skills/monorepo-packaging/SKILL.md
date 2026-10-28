---
name: monorepo-packaging
description: Use when the source archive holds more than one project side by side (a monorepo tarball) — the turn prompt's "Other projects in this archive" section lists more than one entry, or the spec's Name: might be tempted to match just one subdirectory. Covers packaging ALL sibling projects as subpackages of one spec, the %python_subpackages single-invocation limitation, and when an umbrella/metapackage is the right shape instead of re-vendoring an independently-released sibling.
---

# Monorepos

Package ALL sibling projects as subpackages of this one spec. When the "Other
projects in this archive" section in the turn prompt lists more than one
project, the default is ONE source package that builds ALL of them, with a
subpackage per project — not one project built and the rest thrown away, and
not a separate OBS package per subdirectory. They share a tarball, a version
and a release cycle, which is exactly what subpackages are for, and splitting
them would mean checking the same source in several times.

CRITICAL — the source package name and the subpackages:
- The top-level `Name:` field stays as given (e.g. `python-cuda-python`).
  Do NOT rename it to match one of the subprojects.
- Every relevant subproject becomes a `%package -n %{name}-<sub>`
  section (or the appropriate openSUSE Python subpackage macro). None of
  them replaces or is promoted to the top-level package.
- The top-level package itself may be a metapackage (empty %files, only
  Requires on each subpackage) if there is no module at the root level.
- Do NOT create a separate OBS package for a subproject that belongs here —
  EXCEPT a sibling that is independently versioned and published on PyPI
  under its own name (the `cuda-bindings` / `cuda-core` / `cuda-pathfinder`
  family of NVIDIA/cuda-python). Those ARE separate OBS packages built from
  the same tarball: the spec for `python-cuda-bindings` builds only its own
  subdirectory (`cd cuda_bindings` in each section) and lists its siblings as
  ordinary `%{python_module cuda-pathfinder}` style dependencies. Writing
  `Name: python-cuda-bindings` inside that archive's spec is correct, not a
  mistake to restructure away.

The shape:
- %build and %install change into each project directory in turn and run
  %pyproject_wheel / %pyproject_install there. Keep them in dependency
  order if one subproject builds against another.
- each project gets its own subpackage with its own %files listing only
  that project's module directory and dist-info, its own Summary and
  %description, and Requires on its siblings where the code imports them.
- a metapackage subproject (one with a pyproject.toml, no module of its
  own, and dependencies on the others) becomes a subpackage with an empty
  %files and Requires on each sibling.
- NEVER add BuildRequires for a sibling subpackage defined in this same
  spec. Siblings are built and installed into the buildroot before %check
  runs; they are always present. A BuildRequires on a sibling is a circular
  dependency OBS cannot resolve. Use PYTHONPATH=%{buildroot}%{python_sitearch}
  (or sitelib) in %check to make sibling modules importable instead.
Check the exact subpackage and %files macro spelling with the Grep tool
over `/usr/lib/rpm/macros.d`; the pipeline validates the spec after your
turn. Do not guess at macro names.

This is the usual answer, not a rule. Leave a project out, and say in your
reasoning that you did, when:
- it is not a shipped artefact — examples/, benchmarks/, docs/, a test
  harness, an internal build helper;
- it is a different product that merely shares the repository, with its own
  version and no dependency either way;
- it cannot build here at all, in which case package the ones that can
  rather than failing the whole spec.
Never package a subproject you have not opened: read each pyproject.toml
before deciding it is in or out.

**Known macro limitation — do not fight it:** `%python_subpackages` (the
macro that auto-generates the `<flavor>-<name>` subpackages) is bound to
this spec's single top-level `Name:` and can only be invoked once. A
`%package -n python-<other-name>` block does NOT get flavor-multiplied by
it. So when a sibling project is independently versioned and
released on PyPI under its own name (the exception above), and it is
ALSO being packaged as its own separate OBS package (check "Other
projects in this archive" — if a sibling directory declares a name that
sounds like an existing or sibling OBS package, e.g. this spec is
`python-cuda-python` and a sibling declares itself `cuda-bindings`),
do NOT re-vendor that sibling's build inside this spec's subpackages.
Instead make this an umbrella/metapackage: empty (or near-empty) %files,
and `Requires: python-<sibling-name>` for each such sibling instead of
building it a second time. Trying to cram multiple independently-named,
independently-flavored subpackages into one `%python_subpackages`
invocation produces "already exists" errors that no macro argument
fixes — this has already been tried and confirmed a dead end.
