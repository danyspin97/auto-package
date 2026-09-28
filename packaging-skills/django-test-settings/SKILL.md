---
name: django-test-settings
description: Use when a Django-integration test suite fails with "django.core.exceptions.ImproperlyConfigured: settings are not configured" even though Django is already a correctly-installed BuildRequires. Covers exporting DJANGO_SETTINGS_MODULE before %check, and when to gate the Django-dependent tests behind a %bcond instead if Django support is only optional/extra.
---

# Django-integration test suites need `DJANGO_SETTINGS_MODULE`

Symptom: `django.core.exceptions.ImproperlyConfigured: settings are not
configured`, even though `%{python_module Django}` is already a
BuildRequires and installed correctly. This is not a missing-dependency
problem — do not add more BuildRequires to try to fix it.

The test tree ships its own settings module (commonly `tests/settings.py`,
`tests/django_settings.py`, or `tests/test_django/settings.py` — Grep for
`SECRET_KEY =` or `INSTALLED_APPS =` under `tests/` to find the real path)
that Django needs pointed at explicitly:

    export DJANGO_SETTINGS_MODULE=tests.settings
    %pytest

(real examples: `python-Authlib`, `python-drf-standardized-errors`,
`python-testfixtures` — the latter gates the whole Django-dependent test
file behind a `%bcond`, `--ignore`-ing it in the non-Django build instead,
which is the right call when Django support is optional/extra rather than
the package's main purpose).
