1. BuildRequires constraint syntax inside %{python_module ...} — read when writing or fixing a BuildRequires with >=, <, ==
2. Normalized vs display vs import names — read when a Requires/dist-info/%files name has odd casing, dashes or dots
3. Arch-specific vs noarch, C libs and -devel names — read when deciding BuildArch, or an extension links a C/C++ library
4. Merged into convention 6 — read convention 6 when build-backend is maturin or the source has Cargo.toml plus pyproject.toml
5. License must be an SPDX expression — read when License is FIXME, not SPDX, or no LICENSE file exists in the sdist
6. %pyproject_wheel / %pyproject_install for PEP 517 — read when writing %build or %install of a Python package
7. %check with %pytest / %pytest_arch and its BuildRequires — read when adding or editing %check of a Python package
8. Python flavors, %python_expand, %python_files, _multibuild — read when touching flavor macros, multibuild or python_subpackages
9. Never limit parallelism (-j1) — read when the spec or your edit has -j1, -j N, --jobs or -n N on make, cmake, ninja or pytest
10. %check failure: missing dependency vs cannot run here — read when %check fails (ImportError, fixture not found, collect error)
11. Test-skip ladder (deselect, ignore, -o addopts, smoke test) — read when a %check test genuinely cannot run here
12. Merged into convention 7 — read convention 7 when %check fails oddly on config
13. Non-test failures: Source0 mismatch, offline %build errors — read when %prep/%build fails (no such file, name resolution)
14. Tools, quilt and the PATCH-* tag format — read when adding or editing a PatchN:, creating a file, or using Bash (zypper, osc cat, rpm -qlp)
15. Merged into convention 10 — read convention 10 when %check says ModuleNotFoundError or a dep cycle
16. Merged into convention 17 — read convention 17 when writing %files module dirs or an import check for an unfamiliar package
17. Python %files exactness: no globs on the package name — read when writing or fixing %files of a Python package
18. Console scripts and python_alternative — read when the package declares scripts/entry points or %files lists /usr/bin
19. Single % in RPM macros, never %% — read when a spec line shows %% or an rpmspec macro-parse error
20. Merged into convention 19 — read convention 19 when about to use a python macro you have not seen in the spec or a skill
21. Merged into convention 1 — read convention 1 when copying a requirement with extras, markers, commas, ~= or !=
22. Dynamic version/metadata providers need BuildRequires — read when version is 0.0.0 or the build backend import fails
23. Merged into convention 17 — read convention 17 when %files or rpmlint shows tests/docs/setup files at site-packages root
24. Headers: own headers to -devel, vendored ones deleted — read when rpmlint reports devel-file-in-non-devel-package or the package installs .h files
25. rpmlint badness budget (threshold 999) — read when the build log says "Badness NNNN exceeds threshold"
26. Source from a repository, not an sdist — read when the checkout is a git snapshot or the project sits in a subdirectory
27. [inline] Missing dependency: build it, report in unresolvable_packages — full text for naming rules
28. The four exceptions to building a dependency — read when a dependency failed to build, tests need GPU/network, or you return use_upstream_wheel
29. Missing source code (no sdist, no repo URL) — read when no source tarball or repository URL can be found for the package
30. Monorepos: package every sibling project — read when the prompt lists several "Other projects in this archive"
31. rpmlint compliance (summary length, description line length, manpage) — read when rpmlint warns on Summary, description or binaries
32. Prebuilt binary wheel packages (no %build) — read when Source0 is a .whl or the generator says "no sdist" for the release
33. Plain C/C++ packages (CMake/autotools, Skill plain-c-cpp-packaging) — read when the package has no Python and no Cargo
34. Optional extras are Recommends, not BuildRequires — read when pyproject lists optional-dependencies you consider adding
35. Systemd service files and system users — read when the package ships a daemon, a .service unit or needs /run dirs
36. gettext .mo translations (%find_lang) — read when the package installs locale/ .mo files or rpmlint reports them
37. -doc subpackage for Sphinx/generated docs — read when upstream ships docs as an artifact or the task asks for -doc
38. Merged into convention 14 — read convention 14 when tests or code fail with AttributeError/ImportError on a stdlib name
39. rpmlint: fix first; rpmlintrc filter only for a reported warning, # reason:, no setBadness — read when rpmlint reports an E:/W: line or you add addFilter
40. The changelog field: one imperative bullet per change vs the published spec — read when your reply has action edited
41. Obsolete setup.py shims (pytest-runner, setup_requires) — read when setup.py/setup.cfg lists test-runner or shim requirements
42. Hardcoded or placeholder version strings — read when the installed version is wrong or the source has a literal 0.0.0
43. Merged into convention 7 — read convention 7 when pytest ignores options set in setup.cfg
44. Merged into convention 11 — read convention 11 when %check misses a conftest/testdata file or the backend rejects a classifier
45. Merged into convention 10 — read convention 10 when %check says ImproperlyConfigured: settings are not configured
46. Stray shebang in a library file — read when rpmlint reports script-without-shebang/non-executable-script on a module
47. Merged into convention 3 — read convention 3 when a distutils/setuptools Extension ignores distro flags
48. Merged into convention 10 — read convention 10 when %check fails with no missing package
49. Per-CPython upstream wheels covering our flavors — read when the source cannot be built and wheels exist per cp3NN
50. OOM / disk-full failures are sizing, not spec bugs — read when the build is killed or says "No space left on device"
51. Merged into convention 6 — read convention 6 when build-backend is mesonpy or scikit_build_core
52. Merged into convention 2 — read convention 2 when the package is a fork or rename of another PyPI project
53. Optional heavy component as own subpackage (%exclude) — read when one tree holds a light core and a heavy optional part
54. Metapackage subpackage of runtime deps — read when a feature only needs other packages installed, no files
55. Merged into convention 7 — read convention 7 when tests fail only under pytest-xdist -n auto
56. Signed checksums file, GPG verify — read when upstream signs a checksums file (checksum.txt, SHA256SUMS) instead of the tarball
57. Merged into convention 3 — read convention 3 when tests skip or fail because the .so is not in source/
58. Vendored libraries need their LICENSE in %license — read when sources have _vendor/vendor/bundled dirs
59. Exact == pin that openSUSE ships at another version — read when OBS says "nothing provides ... = X (got version Y)"
60. Merged into convention 3 — read convention 3 when choosing BuildArch noarch for a wheel or sdist
61. [inline] A missing dependency may already be built here — full text for rpm -qlp checks and import-name mismatches
62. [inline] Dependency needs patching: patch_dependencies — full text for the two cases and the problem text to write
63. [inline] No TODO placeholders left in a spec — full text for what each skeleton marker needs
64. [inline] C/C++ libs are never python-<lib> — full text; CUDA/NVIDIA vendor wheels are in skill nvidia-wheels
65. Merged into convention 8 — read convention 8 when writing any Requires line for a Python dependency
66. Summary and %description must say something — read when writing Summary/%description or the generator left a stub
67. Merged into convention 11 — read convention 11 when tests need a GPU, a device or network, or %check has more than six --ignore
68. Bundled .so in prebuilt wheels must not leak into dependencies — read when a wheel bundles libs or rpm Requires lists odd .so
69. Merged into convention 5 — read convention 5 when writing %license lines or License:, or a dist-info holds licenses/
70. Optional deps are not Requires; undeclared imports are — read when an extra-only requirement or a runtime import is missing
71. Do not ship what the distro already ships — read before finishing: check Factory for the same import dir (file conflicts)
72. Merged into convention 39 (rpmlintrc filters) — read convention 39 when rpmlint reports an E:/W: line
73. Unsatisfiable bounds (Python or C/C++), siblings first — read when a ==, <, >= bound may exceed what the distro or project has
74. Merged into convention 40 (the changelog field) — read convention 40 when your reply has action edited
75. Version bounds: keep minimums, upper/== only when satisfied, comment when dropped — read when writing a <, <=, == or >= bound in Requires/BuildRequires
76. Files of one Python package come from one tree — read when several wheels fill one package, ModuleNotFoundError on a sibling part
77. Reproducible Source: URLs, one release only — read when adding or editing a Source:, or dropping the old release's archive
78. Merged into convention 5 — read convention 5 when a build refuses over a licence variable
79. ELF RPATH/RUNPATH checks before the first build — read when the package ships .so files or rpmlint reports rpath errors
80. Merged into convention 19 — read convention 19 when %build/%install/%check uses $python or %{$python_*}, or "Unterminated {" appears
81. Merged into convention 10 — read convention 10 when tests may need network, GPU, redis, models or repo files
82. Pre-build reads: requires-python cap, SourceN URLs, cargo build.rs, pybind11 — read when requires-python has <, a SourceN is a URL, or Cargo.lock exists
83. Split wheels sharing a namespace tree: one owner per directory — read when several wheels unpack into one top-level dir
84. CUDA at build time: use the upstream wheel; NVIDIA output is not Factory/SLE material — read when the build or sources mention CUDA, cuda.h, nvcc or libcudart
85. dlopen Requires, cffi minimum, ExclusiveArch propagation, ninja — read when code ctypes-loads a .so, uses cffi, Requires an ExclusiveArch package, or lists ninja
86. [inline] Carried-forward recipe: change only what the release forces; SONAME; no stand-ins; bounds; component lists; header-only noarch devel
87. Merged into convention 7 — read convention 7 when the prompt lists flaky tests or a %check failure differs between two builds
88. Merged into conventions 86 and 10 — read convention 10 when a whole test family fails with empty output
89. SLE, Leap and maintenance targets: minimal change, bug refs — read when the target project is SLE/Leap/maintenance or the spec gates on %sle_version
90. Spec hygiene: no Group:, BuildRoot:, %clean, %defattr; keep header, URL:, SPDX License: — read when a spec has Group:, BuildRoot:, %clean or %defattr, or no URL:
91. Bundled code: unbundle via system option, else Provides: bundled() — read when the sources hold third_party/, vendor/, extern/ or FetchContent
92. Every _service you add uses mode="manual" — read when you write or edit a _service file
93. Upstream .asc/.sig signature: SourceN plus keyring — read when the release page has a .asc, .sig or .sign next to the tarball
94. Merged into convention 14 — read convention 14 when the prompt lists patches that failed to apply or you are about to remove a Patch line
