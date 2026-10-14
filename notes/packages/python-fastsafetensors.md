# python-fastsafetensors

## Build requirements

Checked against 0.4.0 on openSUSE_Tumbleweed, x86_64.

- The upstream sdist builds a C++ pybind11 extension, so it needs a C++ compiler and the package is architecture-specific, not noarch.
- On openSUSE_Tumbleweed only, the pybind11 headers are not under the pip-style include path the build passes; they ship in the pybind11-devel subpackage. Build requires both the pybind11 module and pybind11-devel.
- typer is a declared runtime dependency that the package never imports.
- tqdm and the 3FS reader are guarded or lazy imports, so they are Recommends, not Requires.

## Test changes

Checked against 0.4.0 on openSUSE_Tumbleweed, x86_64.

- The sdist ships no tests, so the check section is only an import smoke test.

## Workarounds

Checked against 0.4.0 on openSUSE_Tumbleweed, x86_64.

- Duplicate-file cleanup targets the sitelib tree, not sitearch.
- The changelog entry is written by hand, because the changelog generation tool fails in the build sandbox on keyring and D-Bus access. A recent fix to session-bus handling may have removed the cause; recheck whether the hand-written entry is still needed.

## Failure history

Checked against 0.4.0 on openSUSE_Tumbleweed, x86_64.

- Compile failed because the pybind11 header was not found. Fixed by adding the pybind11-devel build requirement.
