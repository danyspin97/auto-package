# python-aiomcache notes

## Build requirements
- The package metadata (setup.py install_requires) needs typing_extensions>=4 at runtime, only where the interpreter is older than Python 3.11. Keep the requirement conditional on that marker rather than unconditional.
- Only the runtime requirement is recorded in the package metadata; no test-only requirement for typing_extensions was found.
Checked against 0.8.2 on openSUSE_Tumbleweed, x86_64.

## Test changes
- A %check step imports the package as a smoke test.
Checked against 0.8.2 on openSUSE_Tumbleweed, x86_64.
