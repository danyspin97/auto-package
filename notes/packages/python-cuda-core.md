# python-cuda-core notes

## Build requirements
Checked against 1.2.2 on openSUSE_Tumbleweed, x86_64.
- Upstream ships one wheel per Python flavor, and the set of flavors changes between versions (1.2.1 had cp313 and cp314; 1.2.2 has cp313). The spec must not assume a single flavor.
- Require the cuda-bindings runtime package explicitly. The wheel metadata lists it only under the cu12 and cu13 extras, so a base install does not pull it in, but the module imports it unconditionally and fails without it.

## Test changes
Checked against 1.2.2 on openSUSE_Tumbleweed, x86_64.
- The %check that was hardcoded to the cpython-314 flavor is replaced by an import smoke test of the module, so it runs under whichever flavor is built.

## Workarounds
Checked against 1.2.1 on openSUSE_Tumbleweed, x86_64.
- The generator left the %files block as a TODO; it was filled by hand from the wheel's RECORD listing. For wheel packages the tool should derive the file list from the wheel instead of the spec being hand-edited.
- The generator drops runtime Requires from the wheel metadata. The tool should carry them over instead of relying on a manual edit.
