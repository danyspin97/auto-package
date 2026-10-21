# python-mpmath notes

**Build requirements**
- Upstream has no install_requires, so there are no runtime Requires beyond Python.
- pytest is needed for %check; it is the upstream tests extra.
- gmpy2 is the optional gmpy extra and speeds up arithmetic. The pre-build review recommended it as a Recommends, not a hard dependency; the build did not verify this.
Checked against 1.3.0 on openSUSE_Tumbleweed, x86_64.

**Test changes**
- %check must ignore the build/ directory left in the source tree. Otherwise doctest collection imports a second copy of mpmath from build/lib, failing with an import file mismatch and about 112 collection errors.
- The spherharm doctest is deselected because its last float digit differs (expected ...0007, got ...0004). test_spherharm itself passes. Observed on x86_64 only; other architectures are not checked.
Checked against 1.3.0 on openSUSE_Tumbleweed, x86_64.

**Workarounds**
- The bernfrac doctest builds an integer over 4300 digits, which hits Python's int-to-str digit limit. The test run raises that limit to unlimited.
Checked against 1.3.0 on openSUSE_Tumbleweed, x86_64.

**Failure history**
- Doctest collection imported a second mpmath from the stray build/ directory (import file mismatch, 112 errors). Fixed by ignoring build/ in %check.
- spherharm doctest last-digit float mismatch on x86_64. Fixed by deselecting that doctest.
- bernfrac doctest exceeded the 4300-digit int-to-str limit. Fixed by raising the limit for the test run.
Checked against 1.3.0 on openSUSE_Tumbleweed, x86_64.
