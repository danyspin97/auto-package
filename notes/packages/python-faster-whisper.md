## Build requirements
Checked against 1.2.1 on openSUSE_Tumbleweed, x86_64.
- numpy is in both BuildRequires and Requires: upstream does not declare it, so the spec adds it.
- Upstream upper bounds from requirements.txt are carried into Requires: ctranslate2 below 5, tokenizers below 1, onnxruntime below 2. Keep them in step with requirements.txt when the version changes.

## Test changes
Checked against 1.2.1 on openSUSE_Tumbleweed, x86_64.
- Tests that load a model by name (tiny, tiny.en) fetch it from Hugging Face at run time, which fails in the build because there is no network. These tests are deselected in %check.
- The two offline tests still run, against the installed package on PYTHONPATH rather than the source tree, which has no pyproject.toml.

## Failure history
- 17 of 19 tests failed with LocalEntryNotFoundError caused by a name-resolution error (no network). Fixed by deselecting the model-download tests. Checked against 1.2.1 on openSUSE_Tumbleweed, x86_64.
