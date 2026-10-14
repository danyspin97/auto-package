# python-rich-toolkit

## Build requirements

- Requires (runtime): click, rich, typing-extensions. The package imports them at runtime, and the generator does not add them, so they must be listed by hand.
- BuildRequires (check only): click, rich, typing-extensions, pydantic, inline-snapshot. The test modules import all of them without a guard. The build itself does not need pydantic or inline-snapshot.

Checked against 0.20.6 on openSUSE_Tumbleweed, x86_64.
