//! Cutting a build log into phases.
//!
//! An `osc build` log is three very different documents concatenated:
//!
//! 1. **Setup** — resolving and installing the buildroot. Hundreds of lines,
//!    never once relevant to why a spec failed.
//! 2. **Sections** — `%prep`, `%build`, `%install`, `%check`, each announced by
//!    `Executing(%name): /usr/bin/bash -e /var/tmp/rpm-tmp.XXXXXX`.
//! 3. **Post-build** — `... running NN-check-*`, the install test, the filelist
//!    check and rpmlint. This runs *after* a successful `rpmbuild`, which is
//!    what makes failures here so easy to miss.
//!
//! Knowing which phase a line belongs to is most of the extraction problem.
//! Run 1's iteration 3 is the case that proves it: that build failed with **no
//! `error:` line and no `Bad exit status` anywhere** — the whole diagnosis is a
//! `50-check-filelist` complaint about `/etc/alternatives` being unowned, and
//! anything anchored on rpm's error vocabulary walks straight past it.

use std::sync::LazyLock;

use regex::Regex;

/// Which part of the build a line belongs to.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Phase {
    /// Buildroot resolution and installation. Discardable in its entirety.
    Setup,
    /// An rpm spec section, named with its leading `%`.
    Section(String),
    /// `... running NN-check-*` and everything after it.
    PostBuild,
}

impl Phase {
    /// Whether this phase can be dropped without losing diagnostic signal.
    pub fn is_discardable(&self) -> bool {
        matches!(self, Phase::Setup)
    }

    pub fn name(&self) -> &str {
        match self {
            Phase::Setup => "setup",
            Phase::Section(s) => s,
            Phase::PostBuild => "post-build",
        }
    }
}

/// A contiguous run of lines belonging to one phase.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Span {
    pub phase: Phase,
    /// Inclusive start index into the normalised line slice.
    pub start: usize,
    /// Exclusive end index.
    pub end: usize,
    /// The `/var/tmp/rpm-tmp.XXXXXX` script rpm generated for this section.
    ///
    /// This is the join key back to `Bad exit status from <script> (%section)`,
    /// and it is how a failure gets attributed to a section even when the two
    /// lines are thousands of lines apart.
    pub script: Option<String>,
}

impl Span {
    pub fn len(&self) -> usize {
        self.end.saturating_sub(self.start)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn contains(&self, index: usize) -> bool {
        index >= self.start && index < self.end
    }
}

/// `Executing(%install): /usr/bin/bash -e /var/tmp/rpm-tmp.Asoqjo`
static EXECUTING: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^Executing\((?<section>%\w+)\):(?:.*\s(?<script>/var/tmp/rpm-tmp\.\S+))?")
        .expect("static pattern")
});

/// `... running 50-check-filelist`
static POST_BUILD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\.\.\. running \d+-check-").expect("static pattern"));

/// Split normalised (timestamp-stripped) lines into phase spans.
///
/// Spans are contiguous and cover the whole input, so an index can always be
/// attributed to exactly one phase.
pub fn segment(lines: &[&str]) -> Vec<Span> {
    let mut spans: Vec<Span> = Vec::new();
    let mut current = Span {
        phase: Phase::Setup,
        start: 0,
        end: 0,
        script: None,
    };
    let mut in_post_build = false;

    for (i, line) in lines.iter().enumerate() {
        // Post-build starts at the first check marker and never ends. Later
        // `Executing(...)` lines inside scriptlet output must not reopen a
        // section.
        if !in_post_build && POST_BUILD.is_match(line) {
            current.end = i;
            push_non_empty(&mut spans, current);
            current = Span {
                phase: Phase::PostBuild,
                start: i,
                end: i,
                script: None,
            };
            in_post_build = true;
            continue;
        }

        if in_post_build {
            continue;
        }

        if let Some(caps) = EXECUTING.captures(line) {
            current.end = i;
            push_non_empty(&mut spans, current);
            current = Span {
                phase: Phase::Section(caps["section"].to_string()),
                start: i,
                end: i,
                script: caps.name("script").map(|m| m.as_str().to_string()),
            };
        }
    }

    current.end = lines.len();
    push_non_empty(&mut spans, current);
    spans
}

fn push_non_empty(spans: &mut Vec<Span>, span: Span) {
    if !span.is_empty() {
        spans.push(span);
    }
}

/// Find the span whose generated script matches `script`.
///
/// Used to attribute `Bad exit status from /var/tmp/rpm-tmp.Asoqjo (%install)`
/// to the section that actually produced the output.
pub fn span_for_script<'a>(spans: &'a [Span], script: &str) -> Option<&'a Span> {
    spans.iter().find(|s| s.script.as_deref() == Some(script))
}

/// The span containing a line index.
pub fn span_at(spans: &[Span], index: usize) -> Option<&Span> {
    spans.iter().find(|s| s.contains(index))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shaped exactly like a real log: setup noise, sections, post-build.
    const LOG: &[&str] = &[
        "Building python-glances.spec for standard/x86_64",
        "0.0% cache miss. 46/46 dependencies cached.",
        "Executing(%mkbuilddir): /usr/bin/bash -e /var/tmp/rpm-tmp.aaa",
        "+ umask 022",
        "Executing(%prep): /usr/bin/bash -e /var/tmp/rpm-tmp.bbb",
        "+ cd glances-4.3.0",
        "Executing(%install): /usr/bin/bash -e /var/tmp/rpm-tmp.Asoqjo",
        "copying glances/plugins/foo.py",
        "error: Bad exit status from /var/tmp/rpm-tmp.Asoqjo (%install)",
        "... running 50-check-filelist",
        "python313-glances.rpm: directories not owned by a package:",
        " - /etc/alternatives",
    ];

    #[test]
    fn setup_noise_is_its_own_span() {
        let spans = segment(LOG);
        assert_eq!(spans[0].phase, Phase::Setup);
        assert_eq!(spans[0].start, 0);
        assert_eq!(spans[0].end, 2);
        assert!(spans[0].phase.is_discardable());
    }

    #[test]
    fn sections_are_named_and_carry_their_script() {
        let spans = segment(LOG);
        let install = spans
            .iter()
            .find(|s| s.phase == Phase::Section("%install".into()))
            .expect("%install span");
        assert_eq!(install.script.as_deref(), Some("/var/tmp/rpm-tmp.Asoqjo"));
    }

    #[test]
    fn script_joins_a_failure_back_to_its_section() {
        // The whole reason `script` is captured: the `Bad exit status` line
        // names the script, not the section's output location.
        let spans = segment(LOG);
        let span = span_for_script(&spans, "/var/tmp/rpm-tmp.Asoqjo").expect("joined");
        assert_eq!(span.phase, Phase::Section("%install".into()));
    }

    #[test]
    fn post_build_runs_to_the_end() {
        let spans = segment(LOG);
        let last = spans.last().unwrap();
        assert_eq!(last.phase, Phase::PostBuild);
        assert_eq!(last.end, LOG.len());
        // iteration 3's failure lives here, with no rpm error vocabulary at all.
        assert!(last.contains(LOG.len() - 1));
    }

    #[test]
    fn spans_tile_the_input_without_gaps() {
        let spans = segment(LOG);
        assert_eq!(spans[0].start, 0);
        assert_eq!(spans.last().unwrap().end, LOG.len());
        for pair in spans.windows(2) {
            assert_eq!(pair[0].end, pair[1].start, "gap between spans");
        }
        // …so every line is attributable.
        for i in 0..LOG.len() {
            assert!(span_at(&spans, i).is_some(), "line {i} unattributed");
        }
    }

    #[test]
    fn executing_lines_after_post_build_do_not_reopen_a_section() {
        // rpm scriptlet output in the post-build phase can contain anything,
        // including text that looks like a section marker.
        let log = &[
            "... running 00-check-install-rpms",
            "Executing(%post): /usr/bin/bash -e /var/tmp/rpm-tmp.zzz",
            "update-alternatives: command not found",
        ];
        let spans = segment(log);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].phase, Phase::PostBuild);
    }

    #[test]
    fn a_log_with_no_markers_is_all_setup() {
        let spans = segment(&["just", "some", "lines"]);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].phase, Phase::Setup);
    }
}
