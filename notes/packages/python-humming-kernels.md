# python-humming-kernels

## Build requirements
- BuildRequires: triton, tqdm, tabulate and filelock. Test collection imports `humming`, whose `__init__` imports `humming.ops.bench`, which imports triton; the tests import tqdm and tabulate too. `humming` imports filelock unconditionally, so it is needed at collection as well. Any runtime dependency the tests import at collection needs a BuildRequires as well.
- Requires: filelock, triton, tqdm and tabulate at runtime. filelock is imported at runtime but missing from the upstream dependency list.

Checked against 0.1.16 on openSUSE_Tumbleweed, x86_64.

## Test changes
- Ignore the nine test files that need a CUDA device at collection. Build workers have no GPU and no CUDA headers.
- Deselect by name the 28 device-query tests in test_sm90_h20_heuristics.py and test_tuning_candidates.py that fail for the same reason. The other 18 pass and still run.
- Ignore test_special_paths.py on openSUSE_Tumbleweed x86_64 only: the installed python-torch 2.14.0 lacks `torch._inductor.codegen`, which the test needs.

Checked against 0.1.16 on openSUSE_Tumbleweed, x86_64.

## Workarounds
- The package is noarch and installed under %{python_sitelib}, because the sdist has no compiled extensions. The importable package is `humming`, not `humming_kernels`, so %files must list `humming`.
- SETUPTOOLS_SCM_PRETEND_VERSION is exported so setuptools-scm gets the version without git metadata.
- An rpmlintrc (Source99) filters devel-file-in-non-devel-package for the runtime-JIT csrc sources, which must stay in the runtime package because they are compiled at runtime.

Checked against 0.1.16 on openSUSE_Tumbleweed, x86_64.

## Failure history
- Test collection ImportError (17 errors) on triton, tqdm and tabulate: added them to BuildRequires.
- Test collection failure in nine CUDA-dependent files: ignored those files.
- 28 device-query test failures on missing CUDA headers and GPU: deselected them by name.
- %files failed with a missing directory under the arch-specific site dir: the package is pure Python, so it is installed under python_sitelib and marked noarch.
- rpmlint badness 1100 over the 999 limit from devel-file-in-non-devel-package: filtered in rpmlintrc.

Checked against 0.1.16 on openSUSE_Tumbleweed, x86_64.
