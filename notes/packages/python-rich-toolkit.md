# python-rich-toolkit

## Build requirements

- Requires (runtime): click, rich, typing-extensions. These were missing from the spec and are imported by the package at runtime.
- BuildRequires (test/check only): click, rich, typing-extensions, pydantic, inline-snapshot. The test run needs them; the build itself does not depend on pydantic or inline-snapshot.
- Source extension, dist-info path, noarch architecture, import name, backend name and %files were already correct and needed no change.

Checked against 0.20.6 on openSUSE_Tumbleweed, x86_64.
