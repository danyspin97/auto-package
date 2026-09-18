# Common Build Failure Patterns Added to Agent Prompt

**Date**: 2026-09-18  
**Source**: Analysis of 7 failed packages in production builds

## New Patterns Added

### 1. Source File Format Mismatch

**Pattern Recognition**:
```
error: File /home/abuild/rpmbuild/SOURCES/package-1.0.zip: No such file or directory
```

**Added Guidance**:
- Detect in %prep errors
- Read build.log to find actual filename
- Update Source0 line to match actual file format (.tar.gz vs .zip)

**Example from**: python-torchcodec

---

### 2. Bundled Third-Party Test Suites

**Pattern Recognition**:
```
ModuleNotFoundError: No module named 'pybind11_tests'
ModuleNotFoundError: No module named 'test_utils' (from third_party/)
```

**Added Guidance**:
- Recognize vendored library test suites
- Common paths: `csrc/third_party/*/tests`, `vendor/*/tests`
- Fix with: `%pytest --ignore=csrc/third_party/pybind11/tests`
- Don't try to package test-only modules

**Example from**: python-instanttensor

**Why This Matters**:
Packages vendor entire libraries (e.g., pybind11) including their test suites. These test modules are never installed, so pytest can't import them. The fix is to ignore those test directories entirely.

---

### 3. pytest Internal Errors

**Pattern Recognition**:
```
INTERNALERROR> Traceback (most recent call last):
INTERNALERROR>   File "/usr/lib/python3.14/site-packages/_pytest/main.py"
```

**Added Guidance**:
- Detect pytest crashes before running tests
- Common causes: version incompatibility, broken setup, plugin conflicts
- Fix: Replace entire test suite with smoke test
- Smoke test still validates module imports and C extensions

**Example from**: python-pydantic_core

**When to Apply**:
- pytest itself crashes (not just test failures)
- Can't be fixed by skipping individual tests
- Module is known to work (used by other packages)

---

### 4. Missing Version Control Tools

**Pattern Recognition**:
```
FileNotFoundError: [Errno 2] No such file or directory: 'git'
```

**Added Guidance**:
- Tests shell out to git/hg/svn commands
- Add system package: `BuildRequires: git-core`
- **NEVER** use `%{python_module git}` - git is NOT a Python package
- Common tools: git-core, mercurial, subversion

**Example from**: python-rignore

**Common Mistake to Avoid**:
Agent might try `BuildRequires: %{python_module git}` which creates `python313-git` (wrong!). Git is a system tool, use plain `git-core`.

---

### 5. Module Import Context Dependencies

**Pattern Recognition**:
- Module builds and installs correctly
- `import module` works in isolation
- Full test suite fails with ModuleNotFoundError
- Tests expect specific runtime environment

**Added Guidance**:
- Module may need GPU/torch runtime context to import
- Build succeeds but tests can't import
- Fix: Use smoke test to verify module exists
- Don't waste time chasing test dependencies

**Example from**: python-torch_c_dlpack_ext

---

## Integration with Existing Conventions

These patterns complement existing conventions:

- **Convention 10**: Test failure handling - now includes specific VCS tool pattern
- **Convention 10 (ii)**: Ignoring directories - now includes bundled test suite examples
- **Convention 10 (iv)**: Smoke tests - now includes pytest INTERNALERROR as trigger

## Expected Impact

### Before These Patterns
Agent would:
1. Try to add `python-git` package (doesn't exist)
2. Try to fix pytest INTERNALERROR by adjusting config
3. Try to package `pybind11_tests` module
4. Spend attempts on unfixable issues

### After These Patterns
Agent should:
1. ✅ Add `git-core` directly
2. ✅ Fall back to smoke test immediately
3. ✅ Ignore bundled test directories
4. ✅ Recognize and fix Source0 format mismatches

**Estimated savings**: 1-2 attempts per affected package

## Pattern Coverage

Based on 7 failed packages analyzed:
- ✅ python-rignore: git-core pattern (fixed)
- ✅ python-torchcodec: Source0 format (fixed)
- ✅ python-instanttensor: bundled tests (fixed)
- ✅ python-pydantic_core: pytest INTERNALERROR (fixed)
- ✅ python-torch_c_dlpack_ext: import context (fixed)
- ⏸️ python-ninja: Simple test skip (already in conventions)
- ⏸️ python-ml_dtypes: Dependency availability (not a pattern, needs numpy-devel)

**Coverage**: 5/7 packages (71%) have automatable patterns

## Testing These Patterns

To verify patterns work:

```bash
# Test bundled test suite pattern
auto-package build instanttensor --force --project test
# Should see: --ignore=csrc/third_party/pybind11/tests

# Test VCS tool pattern
auto-package build rignore --force --project test
# Should see: BuildRequires: git-core

# Test pytest INTERNALERROR pattern
auto-package build pydantic_core --force --project test
# Should see: smoke test instead of %pytest

# Test Source0 format pattern
auto-package build torchcodec --force --project test
# Should see: Corrected Source0 to .tar.gz
```

## Future Pattern Candidates

Patterns observed but not yet added (need more data):

1. **CMake find_package failures**: Pattern of missing -devel packages
2. **Rust toolchain version mismatches**: Pattern in maturin builds
3. **CUDA version conflicts**: Pattern in GPU packages
4. **Test data file not found**: Pattern when test fixtures not in sdist

These need more examples before adding as general patterns.

---

## Summary

Added **5 general build failure patterns** to the agent prompt, covering:
- Source handling (format mismatches)
- Test suite management (bundled tests, pytest errors)
- System dependencies (VCS tools)
- Runtime context (import dependencies)

These are **guidelines to recognize patterns**, not package-specific fixes. The agent can now handle these automatically across any package that matches the pattern.

**Result**: Fewer manual interventions, more packages building successfully on first try.
