# python-sentence-transformers

## Build requirements
- Runtime Requires uses the distro's python-huggingface-hub name, not the PyPI spelling huggingface_hub, so it matches the package the distro ships. Proposed at review (medium confidence); the record shows no build failure that confirms it.
Checked against 5.5.1 on openSUSE_Tumbleweed, x86_64.

## Test changes
- An import-smoke check in %check was proposed at review (medium confidence). The record does not show whether it was kept, and no tests were recorded as deselected or ignored.
Checked against 5.5.1 on openSUSE_Tumbleweed, x86_64.
