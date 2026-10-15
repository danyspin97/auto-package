# python-flashinfer-python

## Build requirements
- The package is built as noarch pure Python and installs the flashinfer import name plus the flashinfer console script.
- BuildRequires and Requires come from the project's requirements file. A duplicate apache-tvm-ffi BuildRequires was removed, and a comma-form version constraint was removed.
- Jinja2, filelock and typing_extensions are imported at runtime but not declared upstream, so they are required explicitly.
Checked against 0.7.0.post1 on openSUSE_Tumbleweed, x86_64.

## Test changes
- Two build-infrastructure test files are ignored: they import the build_backend and tests modules, which are not installed.
- test_prims_ts_import_isolation is deselected: it needs cutlass.experimental and tests/prims_ts, which the source does not ship.
Checked against 0.7.0.post1 on openSUSE_Tumbleweed, x86_64.

## Workarounds
- The License tag includes the licences of the vendored CCCL, CUTLASS and fmt sources.
- Unused scripts in the vendored CCCL tree are removed, and shebangs and executable bits are fixed, to keep rpmlint badness under 999.
Checked against 0.7.0.post1 on openSUSE_Tumbleweed, x86_64.

## Failure history
- Test collection failed on the build_backend and tests imports: the two files are ignored (see Test changes).
- test_prims_ts_import_isolation failed on the missing cutlass.experimental and tests/prims_ts: it is deselected.
- rpmlint badness was over the limit because of unused vendored scripts and shebang/exec-bit problems: the scripts were removed and the shebangs and exec bits fixed.
Checked against 0.7.0.post1 on openSUSE_Tumbleweed, x86_64.
