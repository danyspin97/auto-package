---
name: doc-subpackage
description: Use when deciding whether to split a package's own Sphinx/generated documentation into a -doc subpackage — worth doing when upstream itself treats docs as a shipped artifact (a docs extra in [project.optional-dependencies], a Factory sibling package already doing this, or the task explicitly asks for it), NOT speculatively just because a docs/ directory exists in the sdist. Covers building with sphinx-build, deduplicating with %fdupes, and the always-noarch subpackage declaration.
---

# `-doc` subpackage for a package's own Sphinx/generated documentation

Worth doing when upstream itself treats docs as a shipped artifact (a
`docs` extra in `[project.optional-dependencies]`, a Factory sibling
package already doing this, or the task explicitly asks for it) — not
speculatively just because a `docs/` directory exists in the sdist; most
packages ship one without expecting a subpackage. `cookiecutter` is the
reference shape:

- `BuildRequires: python3-Sphinx` plus every extension the docs' own
  `conf.py` imports (`sphinx_rtd_theme`, `myst-parser`,
  `sphinx-autodoc-typehints`, etc. are common — read `conf.py`'s
  `extensions = [...]` list, do not guess).
- Build in %build, after %pyproject_wheel:
    pushd docs
    sphinx-build -b html -d .build/doctrees . _build/html
    rm _build/html/.buildinfo
    popd
- Install in %install, deduplicating by hand since `%doc` can't dedupe a
  whole directory tree the way `%fdupes` does:
    mkdir -p %{buildroot}%{_docdir}/<name>-doc
    cp -r docs/_build/html %{buildroot}%{_docdir}/<name>-doc/
    %fdupes %{buildroot}%{_docdir}/<name>-doc
- Declare the subpackage and its own noarch-ness explicitly — HTML docs
  have no compiled content, so `-doc` is `BuildArch: noarch` even when the
  base package is architecture-specific (a compiled extension). `cookiecutter`
  itself is already `BuildArch: noarch` at the top level, so its `-doc`
  subpackage inherits that for free and doesn't need the line spelled out;
  for a compiled, arch-specific base package, `python-lxml`'s
  `%package -n python-lxml-doc` shows the line written explicitly, which is
  the case that actually needs it:
    %package -n <name>-doc
    Summary:        Documentation for <name>
    BuildArch:      noarch
- `%files -n <name>-doc` needs only `%license <file>` and
  `%doc %{_docdir}/<name>-doc`.
