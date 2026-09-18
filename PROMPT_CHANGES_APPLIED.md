# Applied Prompt Improvements to agent.rs

## Changes Made (2024-09-18)

Based on analysis of 25 package notes.md files, the following improvements have been applied to `/home/danyspin97/proj/coding/auto-package/build/src/agent.rs`:

### 1. Review Task: Added "CRITICAL BUGS FIRST" Section

**Location**: Review task description (lines ~1079-1090)

**Added**:
- **Step 0a**: Check for duplicated dist-info paths (found in ~90% of packages)
  - Generator bug: `package-%{version}.dist-info/package-%{version}.dist-info/`
  - Must be fixed to: `package-%{version}.dist-info/`

- **Step 0b**: Maturin package detection (missed in ~15% of packages)
  - Grep for `build-backend.*maturin` in source/pyproject.toml
  - If found, apply full convention 4 treatment before anything else

**Impact**: Catches the two most common generator bugs before wasting a build attempt.

### 2. Review Task: Reordered Checklist by Priority

**Changes**:
- Moved **import name verification** to step 1 (was buried in old step 1)
  - Now emphasizes checking `*.egg-info/top_level.txt` first
  - Never derive from PyPI package name

- Elevated **console scripts** to step 2 (was step 5)
  - Added explicit validation: verify ALL 7 components are present
  - Lists each required component with clear labels

- Split **test dependencies** into step 3 with TWO sources:
  - a. DECLARED dependencies (from pyproject.toml)
  - b. UNDECLARED imports (grep test files)

- Added **%fdupes target validation** as step 6
  - Must match package type (sitearch vs sitelib)
  - Common generator error

- Added **%pytest vs %pytest_arch** as step 7
  - Arch packages MUST use %pytest_arch
  - Using wrong one causes test failures

**Impact**: Catches import name errors (40% of packages) before test dependency resolution.

### 3. Convention 3 (Architecture): Enhanced with Examples

**Added**:
- "CRITICAL requirements for arch packages" section with 4 mandatory items
- Common -devel package requirements list:
  - pybind11-devel (not plain pybind11)
  - %{python_module numpy-devel}
  - cmake, gcc-c++
  - cuda-devel, cuda-runtime
- Emphasized %pytest_arch requirement
- Emphasized %fdupes must target sitearch (not sitelib)

**Impact**: Prevents the ~25% of packages with wrong %fdupes target or missing -devel packages.

### 4. Convention 14 (Console Scripts): Complete Rewrite

**Changes**:
- Changed from "add these sections" to "add ALL SEVEN components"
- Numbered each component explicitly (Component 1-7)
- Added "CRITICAL: ALL components are MANDATORY" warning
- Listed what happens when each component is missing
- Expanded example to show all sections together
- Added note: "The generator often omits some or all of these"

**Impact**: Addresses the 35% of packages with incomplete console script treatment.

### 5. Convention 17 (BuildRequires): Added WRONG/RIGHT Examples

**Added**:
- Four concrete WRONG/RIGHT example pairs at the top
- New rule: `~=` (compatible release) not supported
- New rule: Spaces around operators REQUIRED
- Added capitalization note (Pillow, PyYAML)
- Reorganized rules with clearer hierarchy

**Examples added**:
```
WRONG: %{python_module maturin >= 1.14.1, < 2.0}
RIGHT: %{python_module maturin >= 1.14.1}

WRONG: %{python_module numpy ~= 2.0}
RIGHT: %{python_module numpy >= 2.0}
```

**Impact**: Prevents the 20% of packages with invalid BuildRequires syntax.

### 6. Workflow Section: Strengthened Evidence Requirements

**Changed**:
- Evidence field now requires "SPECIFIC files/patterns"
- Added REQUIRED format with 4 example entries
- Explicitly banned generic entries like "checked the source"
- Added rule: "If you edited based on a file, that file MUST appear in evidence"

**Impact**: Forces actual investigation instead of assumptions.

## Test Validation

All tests pass:
- `conventions_are_numbered_consecutively_from_one` ✓
- `convention_cross_references_resolve` ✓
- `a_monorepos_sibling_projects_reach_the_prompt` ✓
- `a_single_project_archive_says_nothing_about_subpackages` ✓

## Expected Impact

Based on the 25 packages analyzed:

### Build Attempt Reduction
- **Duplicated dist-info**: Saves 1 attempt per package (~90% of packages)
- **Maturin detection**: Saves 2-3 attempts per maturin package (~15% of packages)
- **Import name**: Saves 1 attempt per package (~40% of packages)
- **Console scripts**: Saves 1-2 attempts per package with scripts (~35% of packages)
- **BuildRequires syntax**: Saves 1 attempt per package (~20% of packages)

**Estimated savings**: 1-3 build attempts per package on average

### Cost Savings
- Fewer build attempts = fewer agent turns
- Each prevented turn saves $0.05-$0.15 in API costs
- 25 packages × 1.5 avg saved attempts × $0.10 = ~$3.75 saved (on this sample alone)

### Quality Improvements
- More specific evidence requirements → better debugging
- Explicit component checklists → fewer missing pieces
- Concrete examples → less ambiguity

### 7. "Missing Dependencies" Section: Universal Packaging Principle

**Added**: New "Missing Dependencies — ALL Types Must Be Packaged" section before workflow

**Key changes**:
- **CRITICAL PRINCIPLE**: ANY missing dependency must be packaged (not just Python)
- Applies to: Python packages, C libraries, system tools, CUDA libraries, everything
- Renamed section from "Missing C Headers and Libraries" to emphasize universality
- Added explicit list of dependency types covered
- Clear guidance: "Do NOT work around missing dependencies by disabling features"

**Enhanced Convention 23**:
- Rewrote as "ANY dependency missing from OBS is something to BUILD"
- Added comprehensive list of dependency types with examples
- Emphasized: "Size and complexity DO NOT MATTER"
- Clarified CUDA special case with step-by-step instructions
- Made exceptions explicit with evidence requirements
- Added: "Keep the spec honest — never remove BuildRequires"

**Impact**: Prevents agents from incorrectly disabling features or skipping tests when dependencies (of any type) are missing. Reinforces that ALL missing dependencies should be reported for packaging.

## Files Modified

1. `/home/danyspin97/proj/coding/auto-package/build/src/agent.rs`
   - Task::Review description (major rewrite)
   - Convention 3 (enhanced)
   - Convention 14 (complete rewrite)
   - Convention 17 (added examples)
   - Convention 23 (major rewrite - universal packaging principle)
   - "Missing Dependencies" section (renamed and expanded)
   - Workflow section (strengthened evidence)

## Files Created

1. `/home/danyspin97/proj/coding/auto-package/PROMPT_IMPROVEMENTS.md`
   - Analysis of 25 package notes.md files
   - Detailed breakdown of top 10 issues
   - Structural improvement suggestions

2. `/home/danyspin97/proj/coding/auto-package/PROMPT_CHANGES_APPLIED.md`
   - This file (summary of applied changes)

## Next Steps

1. **Test on new packages**: Run auto-package on fresh packages to validate improvements
2. **Monitor metrics**: Track build attempt reduction and success rate
3. **Iterate**: Based on new notes.md files, identify any remaining patterns
4. **Consider**: Adding a "Common Fixes" reference section to conventions

## Related Issue

User also noted: `nvidia-nvtx` (wheel-only) vs `nvtx` (has sdist on PyPI) are different packages.
The error about "python-nvidia-nvtx" is for the wheel-only variant, while "python-nvtx" successfully
built from its sdist. This is a source discovery issue, not a prompt issue.
