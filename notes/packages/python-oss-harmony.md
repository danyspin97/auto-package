# python-oss-harmony

## Build requirements
- The package is a maturin-built Rust extension (PyO3), so the Cargo/Rust build environment is needed; maturin is listed once in BuildRequires.
- PyO3 uses the stable ABI from Python 3.8 (abi3-py38), so one built extension covers later Python 3 versions.
- ExclusiveArch restricts the package to tier1 architectures.
- pydantic is declared as a runtime dependency (pyproject requires 2.11.7 or newer) and is also in BuildRequires; the notes do not record why the build needs it.
- The module is installed to the sitearch path and listed in files under its real module name, openai_harmony.
- License is Apache-2.0.
Checked against 0.0.11 on openSUSE_Tumbleweed, x86_64.

## Test changes
- The check step is a real pytest run. No tests are recorded as deselected or ignored.
Checked against 0.0.11 on openSUSE_Tumbleweed, x86_64.

## Workarounds
- Cargo crates come from an offline registry archive shipped as an extra source; the build uses it and does not fetch crates from the network.
- PYO3_PYTHON is set per multibuild flavor, so each flavor builds against its own interpreter.
- The changelog entry was written by hand because the automatic changelog helper failed on keyring access.
Checked against 0.0.11 on openSUSE_Tumbleweed, x86_64.

## Failure history
- License was a FIXME; set to Apache-2.0.
- Summary was too long; shortened.
- Duplicate maturin BuildRequires and a comma-form version constraint removed.
- Wrong noarch/sitelib layout replaced with the sitearch install.
- PyO3 interpreter setup was shared across flavors; now set per flavor in the build step.
Checked against 0.0.11 on openSUSE_Tumbleweed, x86_64.
