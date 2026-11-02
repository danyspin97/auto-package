//! Build-log extraction.
//!
//! Extraction is a precondition, not an optimisation, and the measurement
//! behind that is blunt: a full `osc build` log for a trivial
//! `noarch` package is 138–286 KB, roughly 34k–70k tokens, and the text that
//! actually determined the fix was under 40 lines every time. Anything that
//! compiles C is worse. Sending raw logs would make the log the dominant cost
//! of every attempt *and* bury the signal.

pub mod cause;
pub mod failure;
pub mod line;
pub mod paths;
pub mod phase;

use std::sync::LazyLock;

use regex::Regex;

pub use failure::{Failure, Genre};
pub use line::{has_timestamps, strip_timestamp};
pub use paths::{PathClass, PathMapper};
pub use phase::{Phase, Span};

/// A Python traceback frame.
///
/// Python-only, and deliberately so: the frame grammar is interpreter-specific.
/// A second ecosystem needs its own parser and its own frame type, not a
/// generalisation of this one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PythonFrame {
    pub path: String,
    pub line: u32,
    pub function: Option<String>,
    pub class: PathClass,
}

/// A path the log referred to, with enough context to decide whether to read it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathRef {
    pub raw: String,
    pub class: PathClass,
    /// Depth in the traceback; 0 is the outermost frame. `None` if the path was
    /// found outside a traceback.
    pub frame_depth: Option<usize>,
}

/// The result of extraction: what goes into the request, and nothing else.
#[derive(Debug, Clone, Default)]
pub struct ExtractedLog {
    /// The spec section that failed, e.g. `%install`. The single most useful
    /// routing fact in the whole log.
    pub failed_section: Option<String>,
    /// The terminal error line(s), innermost first.
    pub error_lines: Vec<String>,
    /// Parsed Python traceback frames, outermost first. Empty for a log that
    /// failed some other way — a shell error, a post-build check, a compiler.
    pub python_frames: Vec<PythonFrame>,
    /// Paths worth showing the model, best first.
    pub referenced: Vec<PathRef>,
    /// The excerpt to send, already capped.
    pub excerpt: String,
    /// Last 40 timestamp-stripped lines of the raw log, used as a fallback
    /// when the extractor found no recognisable failure signal (excerpt and
    /// error_lines both empty). Always non-empty for non-empty logs.
    pub log_tail: String,
    /// The located failure: its genre, its phase, and how much was elided.
    /// `None` when nothing in the log looked like a failure at all.
    pub failure: Option<Failure>,
    /// The phase breakdown, kept so callers can quote a different phase without
    /// re-parsing.
    pub spans: Vec<Span>,
    /// True if the log did not look like `osc` output. See [`has_timestamps`].
    pub format_suspect: bool,
}

impl ExtractedLog {
    /// True when the extractor found a recognisable failure signal.
    pub fn has_signal(&self) -> bool {
        self.failure.is_some() && !self.excerpt.is_empty()
    }

    /// Convert to the ap-core event type for event emission.
    pub fn to_event_extract(&self) -> ap_core::event::Extract {
        // A build log is text, so the full disk is read from it here, once.
        let disk_full = self
            .error_lines
            .iter()
            .any(|l| crate::osc::child_output_says_disk_full(l))
            || crate::osc::child_output_says_disk_full(&self.excerpt);
        ap_core::event::Extract {
            genre: self.failure.as_ref().map(|f| f.genre.as_str().to_string()),
            phase: self.failure.as_ref().map(|f| f.phase.name().to_string()),
            located_by: self.failure.as_ref().map(|f| f.located_by.clone()),
            excerpt: if self.has_signal() {
                self.excerpt.clone()
            } else {
                self.log_tail.clone()
            },
            error_lines: self.error_lines.clone(),
            elided_count: self.failure.as_ref().map(|f| f.elided).unwrap_or(0),
            source_candidates: self
                .source_candidates(5)
                .iter()
                .map(std::path::PathBuf::from)
                .collect(),
            has_signal: self.has_signal(),
            disk_full,
        }
    }

    /// The best candidate for "which source file should the model see".
    ///
    /// Run 1 in one call: the innermost frame is in `setuptools`, a dependency
    /// and a dead end; the frame below it is the vendored `fastentrypoints.py`.
    /// Deepest-readable is the rule that picks correctly out of 859 candidates.
    pub fn primary_source_file(&self) -> Option<&str> {
        self.python_frames
            .iter()
            .rev()
            .find_map(|f| match &f.class {
                PathClass::BuildTree { relative } => Some(relative.as_str()),
                _ => None,
            })
    }

    /// Sdist-relative source paths the log pointed at, best first.
    ///
    /// These are **candidates, not an inline set**: the agent reads them with
    /// its own tools when the excerpt is not enough. Inlining them on every
    /// attempt would spend most of the 81× saving extraction buys on attempts
    /// that never needed them.
    pub fn source_candidates(&self, limit: usize) -> Vec<&str> {
        self.candidates(limit, |c| match c {
            PathClass::BuildTree { relative } => Some(relative.as_str()),
            _ => None,
        })
    }

    /// Installed paths under `BUILDROOT`, best first.
    ///
    /// The last rung. These only exist once a build has failed, and they answer
    /// a narrow question — what was actually installed, versus what `%files`
    /// expected. Source explains causes; this only shows consequences.
    pub fn installed_candidates(&self, limit: usize) -> Vec<&str> {
        self.candidates(limit, |c| match c {
            PathClass::BuildRoot { module_path } => Some(module_path.as_str()),
            _ => None,
        })
    }

    /// The section to report to the user and use for routing decisions.
    ///
    /// `failed_section` is `None` when there is no `Bad exit status` line in
    /// the log. That is the normal case for RPM's `%files` check, which is an
    /// internal rpm operation rather than a bash scriptlet. Scanning
    /// `error_lines` for file-check patterns covers that gap.
    pub fn effective_section(&self) -> &str {
        if let Some(s) = &self.failed_section {
            return s;
        }
        if self
            .error_lines
            .iter()
            .any(|l| l.contains("File not found") || l.contains("unpackaged"))
        {
            return "%files";
        }
        "unknown"
    }

    fn candidates<'a>(
        &'a self,
        limit: usize,
        pick: impl Fn(&'a PathClass) -> Option<&'a str>,
    ) -> Vec<&'a str> {
        let mut seen: Vec<&str> = Vec::new();
        for r in &self.referenced {
            if let Some(p) = pick(&r.class)
                && !seen.contains(&p)
            {
                seen.push(p);
                if seen.len() >= limit {
                    break;
                }
            }
        }
        seen
    }
}

static BAD_EXIT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"Bad exit status from \S+ \((?<section>%\w+)\)").expect("static pattern")
});

/// A CPython traceback frame: `  File "…", line 62, in get_args`.
static PY_FRAME: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"^\s+File "(?<path>[^"]+)", line (?<line>\d+)(?:, in (?<func>.+))?"#)
        .expect("static pattern")
});

/// A terminal Python exception line: `AttributeError: 'X' object has no …`.
static PY_EXCEPTION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?<kind>\w+(?:Error|Exception|Warning)): (?<msg>.+)$").expect("static pattern")
});

/// Extract the signal from a raw `osc build` log.
pub fn extract(raw: &str, mapper: &PathMapper) -> ExtractedLog {
    let format_suspect = !has_timestamps(raw);

    // Normalise once. Every anchor below assumes the osc prefix is gone —
    // that is the bug this module exists to not have.
    let lines: Vec<&str> = raw.lines().map(strip_timestamp).collect();
    mapper.scan_log(&lines);

    let spans = phase::segment(&lines);
    let located = failure::locate(&lines, &spans, mapper);

    // Prefer the phase the failure was actually attributed to. `Bad exit
    // status` names a *script*, and `phase` joins that back to the section that
    // produced it; falling back to the raw capture only matters when the
    // section marker is missing from a truncated log.
    let failed_section = located
        .as_ref()
        .and_then(|f| match &f.phase {
            Phase::Section(s) => Some(s.clone()),
            _ => None,
        })
        .or_else(|| {
            lines
                .iter()
                .find_map(|l| BAD_EXIT.captures(l).map(|c| c["section"].to_string()))
        });

    let python_frames = parse_python_frames(&lines, mapper);
    let error_lines = parse_errors(&lines);
    let referenced = rank_paths(&python_frames);
    let excerpt = located
        .as_ref()
        .map(|f| f.excerpt.clone())
        .unwrap_or_default();

    // Always keep a window of the last 40 lines as a fallback, stripped of
    // timestamps so they don't inflate token counts.
    let log_tail = {
        let tail: Vec<&str> = lines.iter().rev().take(40).copied().collect();
        tail.into_iter().rev().collect::<Vec<_>>().join("\n")
    };

    ExtractedLog {
        failed_section,
        error_lines,
        python_frames,
        referenced,
        excerpt,
        log_tail,
        failure: located,
        spans,
        format_suspect,
    }
}

/// Parse CPython traceback frames. Returns empty for any other kind of failure.
fn parse_python_frames(lines: &[&str], mapper: &PathMapper) -> Vec<PythonFrame> {
    lines
        .iter()
        .filter_map(|l| PY_FRAME.captures(l))
        .map(|c| {
            let path = c["path"].to_string();
            let class = mapper.classify(&path);
            PythonFrame {
                line: c["line"].parse().unwrap_or(0),
                function: c.name("func").map(|m| m.as_str().trim().to_string()),
                path,
                class,
            }
        })
        .collect()
}

fn parse_errors(lines: &[&str]) -> Vec<String> {
    let mut out = Vec::new();

    // The Python exception, if there is one, is the most specific statement of
    // what went wrong. Python-only; other ecosystems get nothing from this half
    // and fall through to rpm's verdict below.
    for l in lines.iter().rev() {
        if PY_EXCEPTION.is_match(l) {
            out.push(l.trim().to_string());
            break;
        }
    }

    // Then rpm's own verdict, which is ecosystem-neutral.
    for l in lines {
        if l.starts_with("error: ") {
            out.push(l.trim().to_string());
        }
    }

    out
}

/// Rank the paths a Python traceback mentioned.
///
/// Source first, then installed copies, then dependencies; within a class,
/// deeper frames first, because the innermost frame is nearest the failure.
fn rank_paths(frames: &[PythonFrame]) -> Vec<PathRef> {
    let mut refs: Vec<PathRef> = frames
        .iter()
        .enumerate()
        .map(|(depth, f)| PathRef {
            raw: f.path.clone(),
            class: f.class.clone(),
            frame_depth: Some(depth),
        })
        .collect();

    refs.sort_by(|a, b| {
        a.class
            .priority()
            .cmp(&b.class.priority())
            .then(b.frame_depth.cmp(&a.frame_depth))
    });
    refs.dedup_by(|a, b| a.raw == b.raw);
    refs
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shaped exactly like the run-1 `%install` failure, timestamps intact.
    const RUN1_TAIL: &str = r#"[    7s] Traceback (most recent call last):
[    7s]   File "/usr/lib/python3.14/site-packages/setuptools/_distutils/dist.py", line 1002, in run_command
[    7s]     cmd_obj.run()
[    7s]     ~~~~~~~~~~~^^
[    7s]   File "/usr/lib/python3.14/site-packages/setuptools/command/install_scripts.py", line 50, in _install_ep_scripts
[    7s]     for args in writer.get_args(dist, cmd.as_header()):
[    7s]                 ~~~~~~~~~~~~~~~^^^^^^^^^^^^^^^^^^^^^^^
[    7s]   File "/home/abuild/rpmbuild/BUILD/python-glances-4.3.0-build/glances-4.3.0/fastentrypoints.py", line 62, in get_args
[    7s]     spec = str(dist.as_requirement())
[    7s]                ^^^^^^^^^^^^^^^^^^^
[    7s] AttributeError: 'PathDistribution' object has no attribute 'as_requirement'
[    7s] error: Bad exit status from /var/tmp/rpm-tmp.Asoqjo (%install)
[    7s]
[    7s] RPM build errors:
[    7s]     Bad exit status from /var/tmp/rpm-tmp.Asoqjo (%install)
"#;

    fn extracted() -> ExtractedLog {
        extract(RUN1_TAIL, &PathMapper::with_sdist_root("glances-4.3.0"))
    }

    #[test]
    fn finds_the_failing_section() {
        assert_eq!(extracted().failed_section.as_deref(), Some("%install"));
    }

    #[test]
    fn parses_frames_despite_the_timestamp_prefix() {
        // The whole point. With the prefix left on, this is zero.
        assert_eq!(extracted().python_frames.len(), 3);
    }

    #[test]
    fn picks_the_vendored_file_not_the_dependency() {
        // The innermost frame is setuptools; the answer is one frame out.
        assert_eq!(
            extracted().primary_source_file(),
            Some("fastentrypoints.py")
        );
    }

    #[test]
    fn ranks_source_above_dependencies() {
        let e = extracted();
        assert!(e.referenced[0].class.is_readable_from_sdist());
        assert_eq!(e.source_candidates(8), vec!["fastentrypoints.py"]);
    }

    #[test]
    fn buildroot_paths_are_kept_separate_from_source() {
        // The two rungs must never be conflated: BUILDROOT is the last resort
        // and is not offered alongside source candidates.
        let e = extracted();
        assert!(e.installed_candidates(8).is_empty());
    }

    #[test]
    fn captures_the_exception_and_the_rpm_error() {
        let e = extracted();
        assert!(e.error_lines[0].starts_with("AttributeError:"));
        assert!(e.error_lines.iter().any(|l| l.contains("%install")));
    }

    #[test]
    fn excerpt_is_small() {
        let e = extracted();
        assert!(e.excerpt.lines().count() <= 40);
        assert!(e.excerpt.contains("as_requirement"));
        // Timestamps are stripped on the way out — they are pure token cost.
        assert!(!e.excerpt.contains("[    7s]"));
    }

    /// The failure that comes *after* a successful `%check`.
    ///
    /// The pipeline used to pre-scope the log to the `%check` section before
    /// calling `extract`, which would have cut this failure out entirely. The
    /// whole log goes in; `phase::segment` decides what matters.
    #[test]
    fn a_post_build_failure_after_check_is_still_found() {
        let log = "\
[   22s] Executing(%build): /usr/bin/bash -e /var/tmp/rpm-tmp.aaa
[   24s] Executing(%install): /usr/bin/bash -e /var/tmp/rpm-tmp.bbb
[   27s] Executing(%check): /usr/bin/bash -e /var/tmp/rpm-tmp.ccc
[   30s] 12 passed in 2.1s
[   50s] ... running 50-check-filelist
[   50s] python313-foo.rpm: directories not owned by a package:
[   50s]  - /etc/alternatives
";
        let e = extract(log, &PathMapper::with_sdist_root("foo-1.0"));
        let failure = e.failure.expect("the post-build complaint must be found");
        assert_eq!(failure.genre, Genre::PostBuildCheck);
        assert_eq!(failure.located_by, "post-build-check");
        assert!(failure.excerpt.contains("/etc/alternatives"));
    }

    #[test]
    fn the_extract_records_which_strategy_found_it() {
        let e = extracted();
        assert_eq!(
            e.to_event_extract().located_by.as_deref(),
            Some("rpm-reported-failure")
        );
    }

    #[test]
    fn flags_a_log_that_is_not_osc_output() {
        let e = extract(
            "Traceback (most recent call last):\n",
            &PathMapper::default(),
        );
        assert!(e.format_suspect);
        assert!(!extracted().format_suspect);
    }

    /// A C++ compile failure has no traceback, so the root must be seen in the
    /// compiler's own paths, or the caller would warn that it never matched.
    #[test]
    fn a_compile_log_reports_the_sdist_root_as_matched() {
        let raw = "[    1s] Executing(%build): /usr/bin/bash -e /var/tmp/rpm-tmp.a\n\
[    2s] /home/abuild/rpmbuild/BUILD/duckdb-2.0.0-build/duckdb-2.0.0/src/a.cpp:1:1: fatal error: x.hpp: No such file or directory\n\
[    3s] error: Bad exit status from /var/tmp/rpm-tmp.a (%build)\n";
        let mapper = PathMapper::with_sdist_root("duckdb-2.0.0");
        let e = extract(raw, &mapper);
        assert!(mapper.did_match_root());
        assert_eq!(e.failure.map(|f| f.genre), Some(Genre::CCppCompile));
    }
}
