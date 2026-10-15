# python-aiomcache notes

## Build requirements
- typing_extensions is needed at runtime and at test time, but only where the interpreter is older than Python 3.11.
Checked against 0.8.2 on openSUSE_Tumbleweed, x86_64.

## Test changes
- A %check step imports the package as a smoke test.
Checked against 0.8.2 on openSUSE_Tumbleweed, x86_64.
