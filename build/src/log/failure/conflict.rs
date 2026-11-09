//! The post-build check that finds one file packaged by two subpackages.
//!
//! `50-check-packaged-twice` runs after rpmbuild has succeeded and prints one
//! header per pair of packages, then the paths both of them install:
//!
//! ```text
//! ... running 50-check-packaged-twice
//! found conflict of python313-Kivy-devel-2.3.2 with python314-Kivy-devel-2.3.2:
//!   - /usr/lib64/python3.13/site-packages/kivy/include/config.h
//! ```
//!
//! Nothing in that block is rpm error vocabulary, and the lines around it are
//! test output and logging noise, so the generic strategies find nothing or
//! the wrong thing. This one names the check, and quotes its whole block.

use super::super::phase::Phase;
use super::{BY_PACKAGED_TWICE, CHECK_MARKER, Failure, Genre, MAX_EXCERPT_LINES};

/// `found conflict of A with B:`, the header of one pair of packages.
static CONFLICT: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"^found conflict of \S+ with \S+:$").expect("static pattern")
});

/// The `... running NN-check-packaged-twice` marker, when it is there.
fn is_packaged_twice_marker(line: &str) -> bool {
    CHECK_MARKER.is_match(line) && line.contains("packaged-twice")
}

/// The check's block: every conflict header, with the paths under each, and the
/// marker that starts the check when it sits right before the first header.
pub(super) fn packaged_twice_failure(lines: &[&str]) -> Option<Failure> {
    let first = lines.iter().position(|l| CONFLICT.is_match(l))?;
    let start = if first > 0 && is_packaged_twice_marker(lines[first - 1]) {
        first - 1
    } else {
        first
    };

    // Walk the block: a header, then the indented paths under it, repeated.
    // `end` is the exclusive end of the block so far: a header or an indented
    // path that continues it. Anything else ends the block.
    let mut end = first;
    for (i, line) in lines.iter().enumerate().skip(first) {
        let continues = CONFLICT.is_match(line) || (line.starts_with("  ") && end == i);
        if !continues {
            break;
        }
        end = i + 1;
    }

    let block = &lines[start..end];
    let kept: Vec<&str> = block.iter().take(MAX_EXCERPT_LINES).copied().collect();
    let elided = block.len() - kept.len();
    Some(Failure {
        genre: Genre::PostBuildCheck,
        phase: Phase::PostBuild,
        excerpt: kept.join("\n"),
        elided,
        located_by: BY_PACKAGED_TWICE.to_string(),
    })
}
