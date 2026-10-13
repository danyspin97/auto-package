//! Finding the failure in a segmented log, and quoting just enough of it.
//!
//! Anchoring on a fixed window around `Bad exit status` is not good enough, for
//! two reasons both visible in run 1:
//!
//! - **The anchor is not always there.** Iteration 3 failed with no `error:`
//!   line and no `Bad exit status` at all; the diagnosis was a
//!   `50-check-filelist` complaint in the post-build phase.
//! - **A fixed window is mostly filler.** The 30 lines before iteration 1's
//!   anchor are almost entirely `setuptools/_distutils` plumbing frames that
//!   say nothing about the package being built.
//!
//! So: locate the failure by phase (see [`super::phase`]), classify what kind of
//! failure it is, and quote it according to its kind.

use std::sync::LazyLock;

use regex::Regex;

use super::paths::{PathClass, PathMapper};
use super::phase::{Phase, Span, span_at, span_for_script};

/// Hard ceiling on quoted lines, whatever the genre decides it wants.
const MAX_EXCERPT_LINES: usize = 60;

/// Traceback frames to keep nearest the exception. The innermost frames are
/// where the error actually surfaced.
const TAIL_FRAMES: usize = 4;

/// `+ ` xtrace lines to keep before a shell error — enough to show what ran.
const XTRACE_CONTEXT: usize = 8;

/// Indented continuation lines to keep after a bare `error:`/`warning:` line —
/// enough to show a file list like `Installed (but unpackaged) file(s)
/// found:` actually names, which rpm always puts on the lines *after* the
/// error, never before it.
const MAX_TRAILING_CONTEXT: usize = 20;

/// What kind of failure this is. Determines how it gets quoted, and is a useful
/// clustering key for the cross-package fix cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Genre {
    /// A Python traceback ending in an exception.
    PythonTraceback,
    /// A shell diagnostic from the generated rpm script: command not found,
    /// non-zero exit, and friends.
    ShellError,
    /// A post-build check: filelist, install test, unowned directories,
    /// scriptlet failures. Happens *after* a successful `rpmbuild`.
    PostBuildCheck,
    /// Nothing recognised. The excerpt is a plain window and the model is on
    /// its own; a rising rate here means this module needs another genre.
    Unknown,
}

impl Genre {
    pub fn as_str(self) -> &'static str {
        match self {
            Genre::PythonTraceback => "python-traceback",
            Genre::ShellError => "shell-error",
            Genre::PostBuildCheck => "post-build-check",
            Genre::Unknown => "unknown",
        }
    }
}

/// A located, quoted failure.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Failure {
    pub genre: Genre,
    /// Which phase it happened in — the single most useful routing fact.
    pub phase: Phase,
    /// The quoted lines, already filtered and capped.
    pub excerpt: String,
    /// How many lines were dropped as noise or as elided frames.
    pub elided: usize,
    /// Which strategy in [`locate`] found this failure. Reported to the user at
    /// `--log debug` so a wrong extract can be traced to the rule that produced it.
    pub located_by: String,
}

static BAD_EXIT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^error: Bad exit status from (?<script>\S+) \((?<section>%\w+)\)")
        .expect("static pattern")
});

static TRACEBACK_START: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*Traceback \(most recent call last\):").expect("static"));

/// A CPython traceback frame. Python-only, like [`Genre::PythonTraceback`] and
/// [`collapse_traceback`]; another ecosystem needs its own frame grammar.
static PY_FRAME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"^\s+File "(?<path>[^"]+)", line \d+"#).expect("static"));

/// `/var/tmp/rpm-tmp.Ru643O: line 33: CHOOSE:: command not found`
static SHELL_ERROR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\S+: line \d+: .+$").expect("static"));

/// Post-build complaints that carry no rpm error vocabulary at all.
static POST_BUILD_PROBLEM: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?x)
        scriptlet\ failed
      | directories\ not\ owned\ by\ a\ package
      | Installed\ \(but\ unpackaged\)\ file
      | File\ not\ found
      | \bERROR\b
      ",
    )
    .expect("static")
});

/// `... running 50-check-filelist`
static CHECK_MARKER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\.\.\. running \d+-check-").expect("static"));

/// Build chatter that is never diagnostic.
///
/// Measured on iteration 1: `copying`, `byte-compiling` and `creating` alone
/// account for over half the log's lines.
static NOISE: LazyLock<Regex> = LazyLock::new(|| {
    // `\x20` rather than an escaped literal space: a trailing `\ ` at end of
    // line is silently destroyed by `cargo fmt` stripping trailing whitespace,
    // which turns it into an escaped newline and makes the branch match nothing.
    Regex::new(
        r"(?x)
        ^copying\x20
      | ^creating\x20
      | ^byte-compiling\x20
      | ^adding\x20'
      | ^removing\x20
      | ^writing\x20
      | ^reading\x20
      | ^installing\x20to\x20
      | ^making\x20
      | ^running\x20install
      | ^calling\x20/usr/lib/rpm/
      ",
    )
    .expect("static")
});

/// Whether a line is pure build chatter.
pub fn is_noise(line: &str) -> bool {
    NOISE.is_match(line)
}

/// Locate and quote the failure.
///
/// `lines` must already be timestamp-stripped; `spans` must come from
/// [`super::phase::segment`] over the same slice.
pub fn locate(lines: &[&str], spans: &[Span], mapper: &PathMapper) -> Option<Failure> {
    rpm_reported_failure(lines, spans, mapper)
        .or_else(|| post_build_failure(lines, spans))
        .or_else(|| bare_error_line(lines, spans, mapper))
}

/// Names of the strategies in [`locate`], in the order they are tried.
const BY_RPM_REPORT: &str = "rpm-reported-failure";
const BY_POST_BUILD: &str = "post-build-check";
const BY_BARE_ERROR: &str = "bare-error-line";

/// The common case: rpm names the script that failed, which names the section.
fn rpm_reported_failure(lines: &[&str], spans: &[Span], mapper: &PathMapper) -> Option<Failure> {
    let (idx, caps) = lines
        .iter()
        .enumerate()
        .rev()
        .find_map(|(i, l)| BAD_EXIT.captures(l).map(|c| (i, c)))?;

    let script = &caps["script"];
    let phase = span_for_script(spans, script)
        .map(|s| s.phase.clone())
        .unwrap_or_else(|| Phase::Section(caps["section"].to_string()));

    // Quote from inside the failing section, not from around the rpm error —
    // the two can be far apart, and the section is where the cause is.
    let body = span_for_script(spans, script)
        .map(|s| (s.start, s.end.min(idx + 1)))
        .unwrap_or((idx.saturating_sub(40), idx + 1));

    Some(quote(lines, body, phase, mapper, BY_RPM_REPORT))
}

/// Iteration 3's case: rpmbuild succeeded and the post-build checks objected.
fn post_build_failure(lines: &[&str], spans: &[Span]) -> Option<Failure> {
    let post = spans.iter().find(|s| s.phase == Phase::PostBuild)?;

    let hits: Vec<usize> = (post.start..post.end)
        .filter(|&i| POST_BUILD_PROBLEM.is_match(lines[i]))
        .collect();
    if hits.is_empty() {
        return None;
    }

    let first = *hits.first()?;
    let last = *hits.last()?;

    // Reach back to the `... running NN-check-*` marker so the model is told
    // *which* check objected, not just what it said.
    let marker = (post.start..=first)
        .rev()
        .find(|&i| CHECK_MARKER.is_match(lines[i]))
        .unwrap_or(first);

    let start = marker.min(first.saturating_sub(2));
    let end = (last + 2).min(post.end);

    let kept: Vec<&str> = lines[start..end].to_vec();
    let elided = (end - start).saturating_sub(kept.len());

    Some(finish(
        kept,
        elided,
        Genre::PostBuildCheck,
        Phase::PostBuild,
        BY_POST_BUILD,
    ))
}

/// Last resort: any `error:` line, or `warning:` when there are no errors.
///
/// `rpmspec --parse` exits 0 but emits `warning:` lines for macro problems.
/// Those warnings are the failure signal in that case, so we treat them the
/// same as errors when nothing stronger was found.
fn bare_error_line(lines: &[&str], spans: &[Span], mapper: &PathMapper) -> Option<Failure> {
    let idx = lines
        .iter()
        .rposition(|l| l.starts_with("error: "))
        .or_else(|| lines.iter().rposition(|l| l.starts_with("warning: ")))?;
    let phase = span_at(spans, idx)
        .map(|s| s.phase.clone())
        .unwrap_or(Phase::Setup);
    // rpm often follows a bare `error:`/`warning:` line with the actual
    // detail indented on the lines after it — `Installed (but unpackaged)
    // file(s) found:` is the running example, where the offending paths
    // (the one thing worth fixing %files over) come next, never before.
    let end = lines[idx + 1..]
        .iter()
        .take(MAX_TRAILING_CONTEXT)
        .take_while(|l| l.starts_with(char::is_whitespace))
        .count()
        + idx
        + 1;
    Some(quote(
        lines,
        (idx.saturating_sub(30), end),
        phase,
        mapper,
        BY_BARE_ERROR,
    ))
}

/// Quote a region, choosing the genre from what is actually in it.
fn quote(
    lines: &[&str],
    (start, end): (usize, usize),
    phase: Phase,
    mapper: &PathMapper,
    located_by: &str,
) -> Failure {
    let region = &lines[start..end.min(lines.len())];

    if let Some(tb_start) = region.iter().rposition(|l| TRACEBACK_START.is_match(l)) {
        let (kept, elided) = collapse_traceback(&region[tb_start..], mapper);
        return finish(kept, elided, Genre::PythonTraceback, phase, located_by);
    }

    if let Some(err) = region.iter().rposition(|l| SHELL_ERROR.is_match(l)) {
        let from = err.saturating_sub(XTRACE_CONTEXT);
        let kept: Vec<&str> = region[from..=err].to_vec();
        return finish(kept, from, Genre::ShellError, phase, located_by);
    }

    // Nothing shaped like a traceback or shell error: drop the chatter and
    // keep the tail. rpm's own post-install checks (unpackaged files,
    // scriptlet failures, unowned directories) have a recognisable
    // vocabulary of their own wherever they land in the log — not only
    // inside an explicit OBS `... running NN-check-*` span, which is all
    // `post_build_failure` above can see. rpm's unpackaged-file check in
    // particular runs as part of core `rpmbuild` right after the last
    // scriptlet section (tagged with whatever section that was, e.g.
    // `%license`), never inside one of those spans, so without this check
    // it always fell through to `bare_error_line` and got mislabelled
    // Unknown instead of PostBuildCheck.
    let mut kept: Vec<&str> = Vec::new();
    let mut elided = 0;
    for line in region {
        if is_noise(line) {
            elided += 1;
        } else {
            kept.push(line);
        }
    }
    let genre = if region.iter().any(|l| POST_BUILD_PROBLEM.is_match(l)) {
        Genre::PostBuildCheck
    } else {
        Genre::Unknown
    };
    finish(kept, elided, genre, phase, located_by)
}

/// Keep the frames that say something; elide runs of interpreter plumbing.
///
/// Run 1's traceback is 14 frames, 12 of them inside `setuptools`. The two that
/// matter are the package's own `setup.py` and the vendored
/// `fastentrypoints.py`, so the rule is: always keep frames in the source tree,
/// always keep the last few, and collapse whatever is left.
fn collapse_traceback<'a>(block: &[&'a str], mapper: &PathMapper) -> (Vec<&'a str>, usize) {
    // Group the block into the header, one group per frame, and the tail.
    let mut groups: Vec<(Option<PathClass>, Vec<&'a str>)> = Vec::new();
    let mut current: (Option<PathClass>, Vec<&'a str>) = (None, Vec::new());

    for line in block {
        if let Some(caps) = PY_FRAME.captures(line) {
            groups.push(std::mem::take(&mut current));
            current = (Some(mapper.classify(&caps["path"])), Vec::new());
        }
        current.1.push(line);
    }
    groups.push(current);

    let frame_count = groups.iter().filter(|(c, _)| c.is_some()).count();
    let last_frame = groups.iter().rposition(|(c, _)| c.is_some()).unwrap_or(0);

    let mut kept = Vec::new();
    let mut elided = 0;
    let mut eliding = false;

    for (i, (class, body)) in groups.iter().enumerate() {
        let is_frame = class.is_some();
        let keep = !is_frame
            // Our own source, wherever it appears in the stack.
            || class
                .as_ref()
                .is_some_and(PathClass::is_readable_from_sdist)
            // The frames nearest the exception.
            || i + TAIL_FRAMES > last_frame
            // Short tracebacks are not worth collapsing at all.
            || frame_count <= TAIL_FRAMES + 1;

        if keep {
            if eliding {
                eliding = false;
            }
            kept.extend(body.iter().copied());
        } else {
            if !eliding {
                kept.push("  … interpreter frames elided …");
                eliding = true;
            }
            elided += body.len();
        }
    }

    (kept, elided)
}

fn finish(kept: Vec<&str>, elided: usize, genre: Genre, phase: Phase, located_by: &str) -> Failure {
    let overflow = kept.len().saturating_sub(MAX_EXCERPT_LINES);
    let capped = if overflow > 0 {
        // Keep the end: the exception and rpm's verdict live there.
        &kept[kept.len() - MAX_EXCERPT_LINES..]
    } else {
        &kept[..]
    };

    Failure {
        genre,
        phase,
        excerpt: capped.join("\n"),
        elided: elided + overflow,
        located_by: located_by.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::super::phase::segment;
    use super::*;

    #[test]
    fn error_count_summary_is_not_a_post_build_problem() {
        assert!(!POST_BUILD_PROBLEM.is_match("Summary: 0 ERRORS, 2 warnings"));
        assert!(POST_BUILD_PROBLEM.is_match("ERROR: broken"));
    }

    fn mapper() -> PathMapper {
        PathMapper::with_sdist_root("glances-4.3.0")
    }

    fn run(lines: &[&str]) -> Failure {
        let spans = segment(lines);
        locate(lines, &spans, &mapper()).expect("a failure")
    }

    /// Iteration 2, verbatim in shape: a shell error with xtrace context.
    #[test]
    fn shell_error_keeps_the_command_that_ran() {
        let f = run(&[
            "Executing(%check): /usr/bin/bash -e /var/tmp/rpm-tmp.Ru643O",
            "+ umask 022",
            "+ cd /home/abuild/rpmbuild/BUILD/python-glances-4.3.0-build",
            "+ cd glances-4.3.0",
            "+ CHOOSE:",
            "/var/tmp/rpm-tmp.Ru643O: line 33: CHOOSE:: command not found",
            "error: Bad exit status from /var/tmp/rpm-tmp.Ru643O (%check)",
        ]);
        assert_eq!(f.genre, Genre::ShellError);
        assert_eq!(f.phase, Phase::Section("%check".into()));
        // The xtrace line is what tells the model the placeholder was executed.
        assert!(f.excerpt.contains("+ CHOOSE:"));
        assert!(f.excerpt.contains("command not found"));
    }

    /// Iteration 3: no `error:` line, no `Bad exit status`, diagnosis in
    /// post-build. The case a naive anchor misses entirely.
    #[test]
    fn post_build_failure_is_found_without_rpm_error_vocabulary() {
        let lines = &[
            "Executing(%install): /usr/bin/bash -e /var/tmp/rpm-tmp.aaa",
            "copying glances/plugins/foo.py",
            "... running 00-check-install-rpms",
            "/var/tmp/rpm-tmp.KodKmb: line 2: update-alternatives: command not found",
            "warning: %post(python313-glances-4.3.0-0.noarch) scriptlet failed, exit status 127",
            "... running 50-check-filelist",
            "python313-glances-4.3.0-0.noarch.rpm: directories not owned by a package:",
            " - /etc/alternatives",
        ];
        assert!(
            !lines.iter().any(|l| l.starts_with("error: ")),
            "fixture must not contain rpm error vocabulary"
        );

        let f = run(lines);
        assert_eq!(f.genre, Genre::PostBuildCheck);
        assert_eq!(f.phase, Phase::PostBuild);
        assert!(f.excerpt.contains("/etc/alternatives"));
        // …and it names which check objected.
        assert!(f.excerpt.contains("check-install-rpms") || f.excerpt.contains("check-filelist"));
    }

    #[test]
    fn traceback_keeps_our_source_and_elides_plumbing() {
        let mut lines = vec!["Executing(%install): /usr/bin/bash -e /var/tmp/rpm-tmp.Asoqjo"];
        lines.push("Traceback (most recent call last):");
        lines.push(
            r#"  File "/home/abuild/rpmbuild/BUILD/python-glances-4.3.0-build/glances-4.3.0/setup.py", line 1, in <module>"#,
        );
        lines.push("    setup()");
        // Ten frames of interpreter plumbing.
        for _ in 0..10 {
            lines.push(
                r#"  File "/usr/lib/python3.14/site-packages/setuptools/_distutils/dist.py", line 1002, in run_command"#,
            );
            lines.push("    cmd_obj.run()");
        }
        lines.push(
            r#"  File "/home/abuild/rpmbuild/BUILD/python-glances-4.3.0-build/glances-4.3.0/fastentrypoints.py", line 62, in get_args"#,
        );
        lines.push("    spec = str(dist.as_requirement())");
        lines.push("AttributeError: 'PathDistribution' object has no attribute 'as_requirement'");
        lines.push("error: Bad exit status from /var/tmp/rpm-tmp.Asoqjo (%install)");

        let f = run(&lines);
        assert_eq!(f.genre, Genre::PythonTraceback);
        assert_eq!(f.phase, Phase::Section("%install".into()));

        // Both of our files survive, wherever they sat in the stack.
        assert!(f.excerpt.contains("setup.py"), "entry point dropped");
        assert!(f.excerpt.contains("fastentrypoints.py"), "culprit dropped");
        assert!(f.excerpt.contains("AttributeError"));
        // The plumbing does not.
        assert!(f.elided > 0, "nothing was elided");
        assert!(f.excerpt.contains("elided"));
        assert!(f.excerpt.lines().count() < lines.len());
    }

    #[test]
    fn short_tracebacks_are_left_intact() {
        // Collapsing a three-frame traceback saves nothing and loses context.
        let lines = &[
            "Executing(%build): /usr/bin/bash -e /var/tmp/rpm-tmp.b",
            "Traceback (most recent call last):",
            r#"  File "/usr/lib/python3.14/site-packages/a.py", line 1, in x"#,
            "    boom()",
            "ValueError: nope",
            "error: Bad exit status from /var/tmp/rpm-tmp.b (%build)",
        ];
        let f = run(lines);
        assert_eq!(f.genre, Genre::PythonTraceback);
        assert_eq!(f.elided, 0);
        assert!(!f.excerpt.contains("elided"));
    }

    /// Real shape from `python-duckdb`: rpm's own unpackaged-file check runs
    /// right after the last scriptlet section (`%license` here), never
    /// inside an OBS `... running NN-check-*` span — so there is no
    /// `Phase::PostBuild` for `post_build_failure` to find, and this used to
    /// fall through `bare_error_line` into `Genre::Unknown` instead of being
    /// recognised as the same kind of failure `post_build_failure` handles.
    #[test]
    fn unpackaged_files_outside_a_postbuild_span_is_still_post_build_check() {
        let lines = &[
            "Executing(%license): /usr/bin/bash -e /var/tmp/rpm-tmp.vgFDg8",
            "+ cp -pr /build/duckdb-1.5.6/LICENSE /buildroot/usr/share/licenses/python313-duckdb",
            "+ exit 0",
            "Provides: python313-duckdb = 1.5.6-3.1",
            "Checking for unpackaged file(s): /usr/lib/rpm/check-files /buildroot",
            "error: Installed (but unpackaged) file(s) found:",
            "   /usr/lib/python3.13/site-packages/_duckdb.cpython-313-x86_64-linux-gnu.so",
        ];
        assert!(
            !lines.iter().any(|l| l.starts_with("... running")),
            "fixture must not contain an OBS check-scriptlet marker"
        );

        let f = run(lines);
        assert_eq!(f.genre, Genre::PostBuildCheck);
        assert!(f.excerpt.contains("Installed (but unpackaged)"));
        assert!(f.excerpt.contains("_duckdb.cpython"));
    }

    #[test]
    fn build_chatter_is_dropped_from_unrecognised_failures() {
        let lines = &[
            "Executing(%install): /usr/bin/bash -e /var/tmp/rpm-tmp.c",
            "copying glances/plugins/a.py -> build/lib",
            "byte-compiling /x/y.py to y.pyc",
            "creating build/lib/glances",
            "something genuinely odd happened",
            "error: Bad exit status from /var/tmp/rpm-tmp.c (%install)",
        ];
        let f = run(lines);
        assert_eq!(f.genre, Genre::Unknown);
        assert_eq!(f.elided, 3, "three chatter lines should have gone");
        assert!(f.excerpt.contains("genuinely odd"));
        assert!(!f.excerpt.contains("byte-compiling"));
    }

    #[test]
    fn excerpt_is_capped() {
        let mut lines = vec!["Executing(%install): /usr/bin/bash -e /var/tmp/rpm-tmp.d"];
        for i in 0..500 {
            lines.push(Box::leak(format!("unrecognised line {i}").into_boxed_str()));
        }
        lines.push("error: Bad exit status from /var/tmp/rpm-tmp.d (%install)");
        let f = run(&lines);
        assert!(f.excerpt.lines().count() <= MAX_EXCERPT_LINES);
        // The cap keeps the end, where the verdict is.
        assert!(f.excerpt.contains("Bad exit status"));
    }

    #[test]
    fn noise_filter_matches_the_real_chatter() {
        for l in [
            "copying glances/plugins/foo.py -> build/lib/glances/plugins",
            "byte-compiling /home/abuild/x.py to x.cpython-314.pyc",
            "creating build/lib/glances",
            "calling /usr/lib/rpm/brp-suse.d/brp-25-symlink",
        ] {
            assert!(is_noise(l), "should be noise: {l}");
        }
        for l in [
            "AttributeError: boom",
            " - /etc/alternatives",
            "error: Bad exit status from /x (%install)",
        ] {
            assert!(!is_noise(l), "should not be noise: {l}");
        }
    }
}
