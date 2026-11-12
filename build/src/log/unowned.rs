//! The paths a build log reports as not owned by any package.
//!
//! `50-check-filelist` names every unowned directory of an rpm under one
//! header, one indented bullet per path:
//!
//! ```text
//! jay-compositor-1.15.0-1.1.x86_64.rpm: directories not owned by a package:
//!  - /usr/share/wayland-sessions
//!  - /usr/share/xdg-desktop-portal
//! ```
//!
//! A log can carry several such blocks, one per rpm. The fix needs all of
//! them: a `%dir` added for the first path only costs a second OBS build.

use std::sync::LazyLock;

use regex::Regex;

use super::line::strip_timestamp;

/// The header that opens a block. Anchored at the end so that only the
/// complete header line matches, not a sentence that happens to contain it.
static HEADER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r": directories not owned by a package:\s*$").expect("static pattern")
});

/// An indented ` - /absolute/path` bullet. The path is everything after the
/// `- ` to the end of the line.
static BULLET: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s+- (/.+)$").expect("static pattern"));

/// Every unowned directory the log reports, across all of its rpms.
///
/// Paths are returned in the order first seen and without duplicates. A block
/// ends at the first line that is not a bullet, so a header with no bullets
/// contributes nothing. CRLF line endings and the `osc` elapsed-time prefix
/// (`[ 123s] `) are both tolerated.
pub fn unowned_dirs(log: &str) -> Vec<String> {
    let mut paths: Vec<String> = Vec::new();
    let mut in_block = false;
    for raw in log.lines() {
        let line = strip_timestamp(raw.trim_end_matches('\r'));
        if HEADER.is_match(line) {
            in_block = true;
        } else if in_block {
            match BULLET.captures(line) {
                Some(caps) => {
                    let path = caps[1].to_string();
                    if !paths.contains(&path) {
                        paths.push(path);
                    }
                }
                None => in_block = false,
            }
        }
    }
    paths
}

#[cfg(test)]
mod tests {
    use super::*;

    const THREE: &str = "\
... running 50-check-filelist
jay-compositor-1.15.0-1.1.x86_64.rpm: directories not owned by a package:
 - /usr/share/wayland-sessions
 - /usr/share/xdg-desktop-portal
 - /usr/share/xdg-desktop-portal/portals
";

    #[test]
    fn collects_all_three_paths_in_order() {
        assert_eq!(
            unowned_dirs(THREE),
            [
                "/usr/share/wayland-sessions",
                "/usr/share/xdg-desktop-portal",
                "/usr/share/xdg-desktop-portal/portals",
            ]
        );
    }

    #[test]
    fn two_rpms_in_one_log_are_both_read_and_deduplicated() {
        let log = "\
a-1.0-1.x86_64.rpm: directories not owned by a package:
 - /usr/share/a
 - /usr/share/shared
some other line
b-1.0-1.x86_64.rpm: directories not owned by a package:
 - /usr/share/shared
 - /usr/share/b
";
        assert_eq!(
            unowned_dirs(log),
            ["/usr/share/a", "/usr/share/shared", "/usr/share/b"]
        );
    }

    #[test]
    fn header_with_no_bullets_yields_nothing() {
        let log = "\
a-1.0-1.x86_64.rpm: directories not owned by a package:
error: something else
";
        assert!(unowned_dirs(log).is_empty());
    }

    #[test]
    fn a_bullet_path_with_spaces_is_taken_to_end_of_line() {
        let log = "\
a.rpm: directories not owned by a package:
 - /usr/share/with space/dir
";
        assert_eq!(unowned_dirs(log), ["/usr/share/with space/dir"]);
    }

    #[test]
    fn a_block_ends_at_the_first_non_bullet_line() {
        // The indented line after the blank one is not part of the block.
        let log = "\
a.rpm: directories not owned by a package:
 - /usr/share/a

 - /usr/share/orphan
";
        assert_eq!(unowned_dirs(log), ["/usr/share/a"]);
    }

    #[test]
    fn bullets_that_are_not_absolute_paths_end_the_block() {
        let log = "\
a.rpm: directories not owned by a package:
 - /usr/share/a
 - relative/thing
 - /usr/share/after
";
        assert_eq!(unowned_dirs(log), ["/usr/share/a"]);
    }

    #[test]
    fn a_log_with_no_such_block_yields_nothing() {
        let log = "\
Executing(%install): /usr/bin/bash -e /var/tmp/rpm-tmp.x
 - /usr/share/not/a/block
error: Installed (but unpackaged) file(s) found:
   /usr/lib/x.so
";
        assert!(unowned_dirs(log).is_empty());
    }

    #[test]
    fn osc_timestamp_prefixes_and_crlf_are_tolerated() {
        let log = "[ 123s] ... running 50-check-filelist\r\n\
[ 123s] a.rpm: directories not owned by a package:\r\n\
[ 123s]  - /usr/share/one\r\n\
[ 124s]  - /usr/share/two\r\n\
[ 1234s]  - /usr/share/three\r\n";
        assert_eq!(
            unowned_dirs(log),
            ["/usr/share/one", "/usr/share/two", "/usr/share/three"]
        );
    }
}
