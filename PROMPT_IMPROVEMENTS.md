# Auto-Package Prompt Improvements

Based on analysis of 25 package notes.md files, here are the recurring issues and suggested prompt improvements.

## Top Issues Found (by frequency)

### 1. Duplicated dist-info Paths (appears in ~90% of packages)
**Pattern**: Generator creates `package-%{version}.dist-info/package-%{version}.dist-info/`
**Should be**: `package-%{version}.dist-info/`

**Suggestion**: Add explicit check in Review task:
```
CRITICAL FIRST CHECK: The generator has a known bug where it sometimes doubles the dist-info 
path. Before anything else, read the spec's %files section and verify the dist-info path is 
NOT duplicated (e.g., `foo-%{version}.dist-info/foo-%{version}.dist-info/`). If it is, fix 
it immediately to `foo-%{version}.dist-info/`.
```

### 2. Wrong Import Names (appears in ~40% of packages)
**Examples**:
- protobuf → actual import: `google/`
- opencv_python_headless → `cv2/`
- nvidia_ml_py → `pynvml.py`
- z3_solver → `z3/`
- pillow → `PIL/`

**Current**: Convention 13 exists but is buried
**Suggestion**: Make it first priority in Review task checklist, move before test dependencies

### 3. Maturin Detection Failure (appears in ~15% of packages)
**Pattern**: Packages with `build-backend = "maturin"` marked as `BuildArch: noarch`

**Suggestion**: Add to Review task checklist:
```
0. **FIRST: Check if this is a maturin package** — grep source/pyproject.toml for 
   'build-backend.*maturin'. If found, the generator's arch guess is WRONG and the package 
   needs the full convention 4 treatment (Source1, cargo-packaging, ExclusiveArch, CARGO_* 
   env vars, etc.). Apply all maturin requirements before checking anything else.
```

### 4. Missing python_alternative Treatment (appears in ~35% of packages)
**Pattern**: Console scripts declared but missing Requires(post/postun), %python_clone, %post/%postun, or %python_alternative

**Current**: Convention 14 exists
**Suggestion**: Emphasize in Review checklist and add validation step:
```
After reading [project.scripts], verify ALL components are present:
- %python_clone -a in %install (one per script)
- Requires(post): update-alternatives
- Requires(postun): update-alternatives  
- %post with %python_install_alternative (one per script)
- %postun with %python_uninstall_alternative (one per script)
- %python_alternative %{_bindir}/<script> in %files (one per script)
Missing ANY of these will cause the build to fail.
```

### 5. Wrong %fdupes Target (appears in ~25% of packages)
**Pattern**: Using `%{python_sitelib}` in %fdupes for arch-specific packages

**Suggestion**: Add to convention 3:
```
- In %install, %fdupes must match the package type:
  ARCH packages: %python_expand %fdupes %{buildroot}/%{$python_sitearch}
  NOARCH packages: %python_expand %fdupes %{buildroot}/%{$python_sitelib}
```

### 6. Invalid BuildRequires Syntax (appears in ~20% of packages)
**Patterns**:
- `>= 1.0, < 2.0` (comma not supported)
- `numpy>=2.0; python_version>="3.9"` (markers not supported)
- `~= 2.0` (compatible release not supported)

**Current**: Convention 17 exists
**Suggestion**: Add explicit examples at top of convention 17:
```
WRONG: BuildRequires: %{python_module maturin >= 1.14.1, < 2.0}
RIGHT: BuildRequires: %{python_module maturin >= 1.14.1}

WRONG: BuildRequires: %{python_module numpy ~= 2.0}
RIGHT: BuildRequires: %{python_module numpy >= 2.0}
```

### 7. Missing -devel Packages (appears in ~15% of packages)
**Examples**:
- `pybind11` → needs `pybind11-devel` for headers
- `numpy` → needs `numpy-devel` for C extensions (convention 3 mentions this)

**Suggestion**: Add to top of convention 3:
```
Common -devel package requirements for compiled extensions:
- pybind11-devel (for pybind11/pybind11.h)
- %{python_module numpy-devel} (for numpy/arrayobject.h)
- cmake, gcc-c++ (always needed for CMakeLists.txt)
```

### 8. %pytest vs %pytest_arch Mismatch (appears in ~20% of packages)
**Pattern**: Using `%pytest` for packages with C extensions

**Current**: Part of convention 3
**Suggestion**: Make it more prominent:
```
CRITICAL: C extension packages MUST use %pytest_arch, not %pytest, in %check.
Using %pytest on an arch package will cause test failures.
```

### 9. Capitalization Issues (specific but important)
**Example**: `python-pillow` requires capital P: `%{python_module Pillow}`

**Suggestion**: Add to convention on BuildRequires:
```
Package name capitalization MATTERS. Some packages use mixed case:
- Pillow (not pillow)
- PyYAML (not pyyaml)
Check pyproject.toml or PyPI for the canonical spelling.
```

### 10. Missing Test BuildRequires from Actual Imports (appears in ~50% of packages)
**Pattern**: Test files import modules not declared in [project.optional-dependencies]

**Current**: Mentioned in Review task
**Suggestion**: Make it explicit and actionable:
```
3. **Test dependencies** (TWO SOURCES):
   a. DECLARED: Read [project.optional-dependencies] or [dependency-groups] for 'test', 
      'dev', 'testing' keys
   b. UNDECLARED: Grep all test files for imports and cross-reference against declared deps:
      find source/tests -name '*.py' -exec grep -h '^import\|^from' {} + | sort -u
   Add BOTH declared and undeclared test dependencies as BuildRequires.
```

## Structural Prompt Improvements

### Review Task Reordering
Current order starts with architecture check, but most critical issues are:
1. Duplicated dist-info paths (breaks immediately)
2. Maturin detection (changes entire spec structure)
3. Import name (most common generator error)

**Suggested new order**:
```
0. **CRITICAL BUGS FIRST**:
   a. Check %files for duplicated dist-info path — fix immediately if found
   b. Grep source/pyproject.toml for 'build-backend.*maturin' — if found, apply 
      full convention 4 before anything else

1. **Architecture confirmation** (current step 1)
2. **Import name** (current buried in step 1) — check source/*.egg-info/top_level.txt
3. **Console scripts** — if present, verify ALL python_alternative components
4. **Test dependencies** — both declared AND undeclared imports
5. **Pytest configuration** — addopts, testpaths, fail_under
6. **Build-backend extras** — dynamic version, metadata providers, C/C++ libraries
7. **License** — SPDX validation
8. **Sibling projects** — subpackage layout
```

### Add Common Error Patterns Section
Add before "Workflow" section:
```
## Generator Known Issues (fix these immediately when found)

The spec generator has these known bugs. Check for them FIRST:

1. **Duplicated dist-info path**: `package-%{version}.dist-info/package-%{version}.dist-info/`
   → Fix to single path: `package-%{version}.dist-info/`

2. **Maturin packages marked noarch**: Check for build-backend = "maturin"
   → If found, package needs full convention 4 treatment

3. **Wrong import name**: Generator guesses from PyPI name, often wrong
   → Always verify from source/*.egg-info/top_level.txt

4. **Missing console script treatment**: Scripts declared but python_alternative missing
   → Add ALL components per convention 14

5. **sitelib/sitearch mismatch in %fdupes**: Arch packages using sitelib
   → Match to package type (sitearch for arch, sitelib for noarch)
```

### Evidence Requirements Enhancement
Current evidence field is often ignored. Make it more specific:
```
- "evidence": MUST list specific files/patterns you Read or Grepped, e.g.:
  ["Read source/pyproject.toml lines 1-50",
   "Read source/setup.py",
   "Read source/my_package.egg-info/top_level.txt",
   "Grep 'import' in source/tests/*.py",
   "Grep 'console_scripts' in source/"]
  
  Generic entries like "checked the source" are NOT evidence.
  Each evidence item must name a specific file path or grep pattern.
```

## High-Impact Changes (Prioritized)

1. **Add "Critical Bugs First" step** to Review task (catches 90%+ of duplicated dist-info)
2. **Maturin detection** as step 0 (prevents entire rebuild cycle)
3. **Reorder Review checklist** to put import name before test deps
4. **Strengthen console script validation** (missing in 35% of cases)
5. **Add common BuildRequires syntax errors** to convention 17 with examples

## Cost/Benefit Analysis

Most issues caught by these improvements would save:
- 1-2 build attempts per package (avg 3-5 minutes per attempt)
- $0.05-$0.15 in API costs per package
- Reduction in "Fix after build" cycles

The prompt additions are ~500 tokens but would be heavily cached, so marginal cost is minimal.
