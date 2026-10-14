# python-cuda-core notes

## Build requirements
Checked against 1.2.1 on openSUSE_Tumbleweed, x86_64.
- Upstream ships one wheel per Python flavor (cp313 and cp314 for this version). The spec must not assume a single flavor.
- The generator dropped a runtime Requires for this wheel package. It was added by hand; the journal does not record which one, so verify the wheel's runtime dependencies on the next build.

## Test changes
Checked against 1.2.1 on openSUSE_Tumbleweed, x86_64.
- %check was made independent of the Python flavor, so it runs under whichever flavor is built.

## Workarounds
Checked against 1.2.1 on openSUSE_Tumbleweed, x86_64.
- The generator left the %files block as a TODO. It was filled by hand from the wheel's RECORD listing. The tool should derive the file list from the wheel for wheel packages; do not hand-edit the spec.
- The generator dropped the runtime Requires noted above. The tool should carry runtime dependencies over from the wheel metadata instead of relying on a manual edit.
