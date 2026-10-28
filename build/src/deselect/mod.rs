//! Dropping failing tests from `%check`, mechanically, with the log as evidence.
//!
//! When the package builds and installs and only a few individual tests fail in
//! `%check`, the fix is to skip exactly those tests and say why. This reads the
//! failing test ids from the build log (pytest `FAILED path::test` lines,
//! unittest `FAIL: test (mod.Class.test)` headers, ctest's "The following tests
//! FAILED" list) and edits the spec: a narrow `--deselect` through
//! `PYTEST_ADDOPTS`, an `-E` regex on the `%ctest` line, or a `skip` decorator
//! inserted in `%prep` for unittest modules found in the unpacked sdist. A
//! unittest module that is not there (an installed package's tests) is skipped
//! through the runner instead: `%check` is moved to pytest (`-m unittest` is
//! rewritten, pytest added to the BuildRequires) and the test is excluded by a
//! `-k` expression in `PYTEST_ADDOPTS`; a `%check` that runs tox, nox, make
//! and the like is refused with the exact reason. Every dropped test carries a
//! `# evidence:` comment quoting its log line.
//!
//! It refuses when the failure is a dependency problem (an import error, a
//! collection error, no test passing at all) because deselecting would hide a
//! package that does not work. Never a whole file or directory; at most
//! [`MAX_PER_ROUND`] tests at a time and [`MAX_IN_SPEC`] in one spec.

use crate::log::strip_timestamp;

mod apply;
mod parse;
#[cfg(test)]
mod tests;
mod unittest;

pub use apply::*;
pub use parse::*;
use unittest::*;
