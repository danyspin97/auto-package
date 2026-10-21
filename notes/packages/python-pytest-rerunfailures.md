# python-pytest-rerunfailures notes

## Build requirements
- The project uses the setuptools build backend with no explicit package or module list, so setuptools discovers the module. It has no setup.py and only an egg_info section in setup.cfg.
- Runtime dependencies from pyproject: packaging>=17.1 and pytest>=8.2 excluding 8.2.2. Pytest also registers a plugin entry point, so pytest must be present at runtime.
- Checked against 16.7 on openSUSE_Tumbleweed, x86_64 (read from source before the build; build output not recorded in the journal).

## Workarounds
- The generated spec's %files entry ships a `src/` directory under the Python sitelib. The module is a single file under src/ that setuptools installs at the top level of site-packages, so that entry looks wrong. Whether the spec was corrected in this build is not recorded.
- Checked against 16.7 on openSUSE_Tumbleweed, x86_64 (pre-build review; not confirmed by build output).

## Test changes
- Planned to run the xdist tests. Whether any tests were deselected or ignored is not recorded.
- Checked against 16.7 on openSUSE_Tumbleweed, x86_64 (pre-build plan only).
