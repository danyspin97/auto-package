# python-backports-strenum

Version 1.3.1. Last build succeeded on 2026-10-13.

## Build requirements
- Upstream uses the poetry-core backend. The wheel's dist-info directory is `backports_strenum-1.3.1.dist-info/`, and the license paths in %files depend on that name.
- The import tree is the `backports/strenum` namespace package. %files must list `backports/strenum`; `backports_strenum/` fails with "File not found".
- Upstream declares `python = ">=3.8.6,<3.11"`, which becomes Requires-Python and rejects the python314 flavor. See Workarounds.

## Test changes
- %check runs the upstream unittest suite from the v1.3.1 tag, fetched as Source1 (`test_strenum.py`).
- No tests are recorded as deselected, ignored or patched.

## Workarounds
- **Patch0** (quilt, with a PATCH-FIX-OPENSUSE provenance comment): relaxes the Python bound in pyproject.toml to `>=3.8.6` so pip builds for the python314 flavor. Logged in .changes.
- **Source1 fetch**: a `_service` file runs download_files so `osc service ra` fetches `test_strenum.py`. The file is not in the checkout.
- **License**: marked with %license at its dist-info `licenses/` path, with %exclude in the file list.

## Failure history
- Spec would not parse before attempt 1 (cause not recorded). Fixed: `rpmspec --parse` now exits 0.
- %check source missing: `test_strenum.py` was not on disk. Fixed: `_service` download_files entry for Source1.
- %build: pip refused because of the upstream `<3.11` bound on the python314 flavor. Fixed: Patch0.
