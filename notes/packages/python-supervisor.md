# python-supervisor notes

## Build requirements
- The package is noarch and setup.py-only: no pyproject.toml, no compiled code, no runtime dependencies, so no runtime Requires are needed.
- The four console scripts need update-alternatives wiring; the generator does not emit it. This is a gap for the tool to fill, not something to patch by hand in the spec.
- Checked against 4.3.0 on openSUSE_Tumbleweed, x86_64. The review confirmed noarch, no ext_modules, four console_scripts and an empty install_requires.

## Test changes
- No tests are deselected or patched; the upstream test suite passes (1370 tests) for both flavors.
- Checked against 4.3.0 on openSUSE_Tumbleweed, x86_64.

## Workarounds
- Installed tests and sample scripts are removed, together with their RECORD lines. Their files use an interpreter placeholder or /usr/bin/env shebang, which rpmlint's script-interpreter checks flag.
- The three entry-point modules that still carry a /usr/bin/env shebang have that shebang stripped in %prep. Installed without the executable bit, they are flagged as non-executable scripts.
- The license is written as SUSE-Repoze because the upstream license is not an SPDX identifier.
- Checked against 4.3.0 on openSUSE_Tumbleweed, x86_64.

## Failure history
- rpmlint badness 2056 over the threshold of 999 from script-interpreter and non-executable-script errors on tests, sample scripts and entry points: fixed by the removals and shebang strip under Workarounds. Checked against 4.3.0 on openSUSE_Tumbleweed, x86_64.
