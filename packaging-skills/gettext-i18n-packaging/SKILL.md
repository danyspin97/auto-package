---
name: gettext-i18n-packaging
description: Use when a package installs a locale/ (or similarly named) directory of compiled .mo translation files — common in web-framework-adjacent packages (forms, templating, CLI tools with translated messages). Symptom is "Installed (but unpackaged) file(s) found" naming .mo files under LC_MESSAGES/. Covers %python_find_lang and the directory-rename workaround when translations live under a non-standard directory name.
---

# Translations / gettext .mo files (i18n)

Needs `%find_lang`/`%python_find_lang` to generate the file list, not a
hand-written %files entry per language directory. Symptom:
`Installed (but unpackaged) file(s) found` naming `.mo` files under
`LC_MESSAGES/`, or a language directory swallowed into the package's own
`%{python_sitelib}/<pkg>/` line without being explicitly owned.

- Confirm first with `find %{buildroot} -name '*.mo'` — do not apply this
  speculatively to a package with no compiled translations.
- In %install, after %pyproject_install:
    %python_find_lang <name>
- In %files, pull the generated list instead of the raw path:
    %files %{python_files} -f %{python_prefix}-<name>.lang
- Directory-name mismatch: `%find_lang` looks for a directory literally
  named `locale/`. A package that installs its translations under a
  different name (`i18n/` in `python-FormEncode`) needs a rename dance
  around the macro call rather than a %files miss:
    %python_expand mv %{buildroot}%{$python_sitelib}/<pkg>/{i18n,locale}
    %python_find_lang <name>
    sed -i s/locale/i18n/ python*-<name>.lang
    %python_expand mv %{buildroot}%{$python_sitelib}/<pkg>/{locale,i18n}
- `%python_find_lang` is a real, currently supported macro (defined by
  python-rpm-macros on both openSUSE Tumbleweed and SLE) — use it
  directly; do not invent a local `%define` workaround unless
  `rpmspec --parse` actually reports it undefined.
