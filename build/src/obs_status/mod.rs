//! Reading OBS build status from its XML API, and deciding what it means.
//!
//! `osc results` prints a human table: the verdict of a finished package is
//! hidden (`finished`), staleness is a trailing `*`, and the project-wide
//! layout is positional. Scraping that was the source of several hangs and
//! misreads, so status is read from `/build/<project>/_result` instead and
//! interpreted by [`resolve`], a pure function of the XML plus (only when
//! needed) the source revision and job history.
//!
//! No XML crate is pulled in for this: the documents are small, flat and
//! machine-generated, so a tag scanner with attribute decoding is enough —
//! and it is deliberately lenient (unknown tags/attributes are ignored,
//! attribute order and quote style don't matter, self-closing tags work).

use crate::osc::BuildStatus;

mod parse;
#[cfg(test)]
mod tests;
mod verdict;

pub use parse::*;
pub use verdict::*;
