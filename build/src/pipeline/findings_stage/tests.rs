use super::*;
use crate::agent::{Action, FindingAnswer, FindingStatus};
use crate::bugfix::{
    FindingRoute, FindingsJob, FixTask, MechanicalFix, ReviewFinding, read_fix_end, write_fix_end,
};
use crate::pipeline::findings_rounds::TurnSeams;
use crate::pipeline::hygiene_tests::config;
use crate::request_review::{Origin, Severity};

const NONCE: &str = "n-1";
const SEP: &str = "-------------------------------------------------------------------";
const BASE: &str = "-------------------------------------------------------------------\nMon Jan  1 00:00:00 UTC 2026 - x@y\n\n- Update to 1.0\n\n";
/// Applies to the sources of `source_tarball` (`f.txt` is `a B c d`).
const DIFF: &str = "--- a/f.txt\n+++ b/f.txt\n@@ -1,4 +1,4 @@\n a\n B\n-c\n+C\n d\n";
const SPEC: &str = "Name: foo\nVersion: 1.0\nRelease: 0\nSummary: test\nLicense: MIT\nSource0: foo-1.0.tar.gz\n# PATCH-FIX-UPSTREAM a.patch bsc#1 -- fix\nPatch0: a.patch\n\n%prep\n%autosetup -p1\n";

fn finding(id: &str, severity: Severity, origin: Origin) -> ReviewFinding {
    ReviewFinding {
        id: id.into(),
        severity,
        origin,
        package: "foo".into(),
        file: Some("foo.changes".into()),
        line: Some(4),
        message: "m".into(),
    }
}

fn job(findings: Vec<(ReviewFinding, FindingRoute)>) -> FindingsJob {
    FindingsJob {
        request_id: "77".into(),
        source_project: "home:u:branches:P".into(),
        target_project: "P".into(),
        pkg: "foo".into(),
        version: "1.0".into(),
        findings,
        target_changes: Some(BASE.into()),
        target_files: Vec::new(),
        nonce: NONCE.into(),
    }
}

fn warn(id: &str) -> ReviewFinding {
    finding(id, Severity::Warning, Origin::Check)
}

struct Seen(std::sync::Mutex<Vec<ap_core::fix_event::FindingFixEvent>>);

impl Reporter for Seen {
    fn report(&self, e: &Event) {
        if let Event::FindingFix(f) = e {
            self.0.lock().unwrap().push(f.clone());
        }
    }
}

type Turn = dyn FnMut(&RunCtx, &str) -> crate::agent::TurnOutcome;

struct Case {
    _root: tempfile::TempDir,
    result: Result<(), PipelineResult>,
    events: Vec<ap_core::fix_event::FindingFixEvent>,
    contexts: Vec<String>,
    changes: String,
    checkout: PathBuf,
    ws_dir: PathBuf,
}

fn source_tarball(checkout: &Path) {
    let file = std::fs::File::create(checkout.join("foo-1.0.tar.gz")).unwrap();
    let enc = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
    let mut ar = tar::Builder::new(enc);
    let body = b"a\nB\nc\nd\n";
    let mut h = tar::Header::new_gnu();
    h.set_size(body.len() as u64);
    h.set_mode(0o644);
    h.set_cksum();
    ar.append_data(&mut h, "foo-1.0/f.txt", &body[..]).unwrap();
    ar.into_inner().unwrap().finish().unwrap();
}

fn run(
    no_agent: bool,
    own: &str,
    prior: Option<crate::bugfix::FixEnd>,
    job: FindingsJob,
    turn: &mut Turn,
) -> Case {
    let root = tempfile::tempdir().unwrap();
    let work = root.path().join("work");
    let state_root = work.join("state");
    let mut config = config("foo", &state_root);
    config.rpm_name = "foo".into();
    config.work_dir = work.clone();
    config.project = "proj".into();
    config.no_agent = no_agent;
    config.budget_usd = 100.0;
    config.max_attempts = 10;
    config.version = "1.0".into();
    let ws = Workspace::open(&state_root, "foo").unwrap();
    let checkout = work.join("proj/foo");
    std::fs::create_dir_all(checkout.join(".osc")).unwrap();
    source_tarball(&checkout);
    let spec_path = checkout.join("foo.spec");
    std::fs::write(&spec_path, SPEC).unwrap();
    std::fs::write(
        checkout.join("a.patch"),
        format!("Subject: fix\nReferences: bsc#1\n\n{DIFF}"),
    )
    .unwrap();
    std::fs::write(checkout.join("foo.changes"), format!("{own}{BASE}")).unwrap();
    if let Some(end) = &prior {
        write_fix_end(&ws.dir(), end).unwrap();
    }
    let run = ap_core::run::RunDir::allocate(&work).unwrap();
    let pkg_run = run.pkg("foo");
    let seen = Seen(Default::default());
    let ctx = RunCtx {
        config: &config,
        pkg_run: &pkg_run,
        reporter: &seen,
        ws,
        rpm_name: "foo".into(),
        obs_name: "foo".into(),
        checkout_dir: checkout.clone(),
        spec_path,
        signals: Default::default(),
    };
    let mut st = RunState {
        step_attempts: Default::default(),
        sdist_top_level: String::new(),
        from_repo_archive: false,
        attempts: 0,
        spent_usd: 0.0,
        session_id: None,
        last_log_digest: None,
        identical_logs: 0,
        same_shape_failures: 0,
        build_rounds: Default::default(),
        last_failure_shape: None,
        relaxed_from_spec: None,
        post_review_done: false,
        run_start_spec: None,
        post_review_revert: None,
        rpmlint_shown: Default::default(),
        update: None,
        carried: None,
    };
    let step_path = root.path().join("step");
    std::fs::create_dir_all(&step_path).unwrap();
    let mut contexts: Vec<String> = Vec::new();
    let mut run_turn = |ctx: &RunCtx, _: &mut RunState, _: usize, context: &str| {
        contexts.push(context.to_string());
        let out = turn(ctx, context);
        Ok((
            StepDir {
                path: step_path.clone(),
                id: ap_core::event::StepId(2),
            },
            out,
        ))
    };
    let mut applies = |_: &RunCtx, _: &str| Ok(());
    let mut review = |_: &RunCtx, _: &mut RunState, _: &str, _: &str| panic!("no patch review");
    let task = FixTask::Findings(job);
    let FixTask::Findings(job) = &task else {
        unreachable!()
    };
    let result = findings_stage_with(
        &ctx,
        &mut st,
        job,
        FindingsSeams {
            turns: TurnSeams {
                turn: &mut run_turn,
                applies: &mut applies,
                review: &mut review,
            },
        },
    );
    Case {
        _root: root,
        result,
        events: seen.0.lock().unwrap().clone(),
        contexts,
        changes: std::fs::read_to_string(checkout.join("foo.changes")).unwrap(),
        ws_dir: ctx.ws.dir().to_path_buf(),
        checkout,
    }
}

fn own_entry(bullets: &str) -> String {
    format!("{SEP}\nTue Nov  3 11:14:44 UTC 2026 - D <d@s.com>\n\n{bullets}\n\n")
}

fn outcome(action: Action, answers: Vec<(&str, FindingStatus)>) -> crate::agent::TurnOutcome {
    let mut out = pipeline_wheel_outcome("x");
    out.action = action;
    out.finding_answers = answers
        .into_iter()
        .map(|(id, status)| FindingAnswer {
            id: id.into(),
            status,
            reason: format!("{id} reason"),
        })
        .collect();
    out
}

fn failed(case: &Case) -> (&FailureKind, &str) {
    match &case.result {
        Err(PipelineResult::Failed { kind, reason, .. }) => (kind, reason),
        other => panic!("{other:?}"),
    }
}

fn outcomes(case: &Case) -> Vec<(&str, &str, &str)> {
    case.events
        .iter()
        .map(|e| (e.finding.as_str(), e.route.as_str(), e.outcome.as_str()))
        .collect()
}

#[test]
fn a_tool_only_job_makes_the_fix_and_asks_no_model() {
    let own = own_entry("- Update to 1.0\n- Refresh a.patch\n- Refresh b.patch");
    let findings = vec![
        (
            warn("changes.refresh-not-grouped"),
            FindingRoute::Tool(MechanicalFix::RefreshBullets),
        ),
        (warn("build.pending"), FindingRoute::Wait),
        (
            warn("risk.new_scriptlet"),
            FindingRoute::Maintainer("by design"),
        ),
        (
            finding("files.leftover", Severity::Note, Origin::Check),
            FindingRoute::NotActedOn,
        ),
    ];
    let case = run(false, &own, None, job(findings), &mut |_, _| {
        panic!("no turn")
    });
    assert!(case.result.is_ok(), "{:?}", case.result);
    assert!(
        case.changes
            .contains("- Refresh patches:\n  * a.patch\n  * b.patch\n"),
        "{}",
        case.changes
    );
    assert!(case.changes.ends_with(BASE));
    assert_eq!(
        outcomes(&case),
        [
            ("changes.refresh-not-grouped", "tool", "fixed"),
            ("build.pending", "wait", "not_fixed"),
            ("risk.new_scriptlet", "maintainer", "maintainer"),
            ("files.leftover", "not_acted_on", "not_acted_on"),
        ]
    );
    assert_eq!(read_fix_end(&case.ws_dir, NONCE).unwrap().verdict, "fixed");
}

#[test]
fn a_tool_fix_that_refuses_goes_to_a_person_and_nothing_is_changed() {
    // The header has no mail: the tool does not guess one.
    let own = format!("{SEP}\nTue Nov  3 11:14:44 UTC 2026 - nobody\n\n- Update to 1.0\n\n");
    let findings = vec![(
        warn("changes.header-format"),
        FindingRoute::Tool(MechanicalFix::HeaderFormat),
    )];
    let case = run(false, &own, None, job(findings), &mut |_, _| {
        panic!("no turn")
    });
    assert_eq!(*failed(&case).0, FailureKind::NoFix);
    assert_eq!(
        outcomes(&case),
        [("changes.header-format", "tool", "maintainer")]
    );
    assert_eq!(case.changes, format!("{own}{BASE}"));
}

#[test]
fn an_agent_finding_is_one_turn_with_the_finding_fenced_as_data() {
    let own = own_entry("- Update to 1.0");
    let findings = vec![(warn("patch.header"), FindingRoute::Agent)];
    let case = run(false, &own, None, job(findings), &mut |_, _| {
        outcome(
            Action::NoChange,
            vec![("patch.header", FindingStatus::NotFixed)],
        )
    });
    assert_eq!(case.contexts.len(), 1);
    let ctx = &case.contexts[0];
    assert!(
        ctx.contains("request 77") && ctx.contains("<untrusted-data"),
        "{ctx}"
    );
    assert!(
        ctx.contains("[warning] patch.header foo.changes:4: m"),
        "{ctx}"
    );
    // The turn changed nothing, so the run has nothing to build.
    assert_eq!(*failed(&case).0, FailureKind::NoFix);
    assert_eq!(outcomes(&case), [("patch.header", "agent", "not_fixed")]);
    assert_eq!(case.events[0].reason, "patch.header reason");
}

#[test]
fn an_edit_of_a_patch_header_is_accepted_and_recorded_as_fixed() {
    let own = own_entry("- Update to 1.0\n- Add a.patch: fix");
    let findings = vec![(warn("patch.header"), FindingRoute::Agent)];
    let case = run(false, &own, None, job(findings), &mut |ctx, _| {
        let p = ctx.checkout_dir.join("a.patch");
        let text = std::fs::read_to_string(&p).unwrap();
        std::fs::write(&p, format!("Upstream: not yet reported\n{text}")).unwrap();
        let mut out = outcome(Action::Edited, vec![("patch.header", FindingStatus::Fixed)]);
        out.changelog = "Add the Upstream header to a.patch".into();
        out.evidence = vec!["Read a.patch".into()];
        out
    });
    assert!(case.result.is_ok(), "{:?}", case.result);
    assert_eq!(outcomes(&case), [("patch.header", "agent", "fixed")]);
    let patch = std::fs::read_to_string(case.checkout.join("a.patch")).unwrap();
    assert!(patch.starts_with("Upstream: not yet reported\n"));
    assert!(
        case.changes.contains("Add the Upstream header to a.patch"),
        "{}",
        case.changes
    );
}

#[test]
fn a_changes_entry_that_drops_a_patch_bullet_is_refused_and_the_turn_retried() {
    let own = own_entry("- Update to 1.0\n- Add a.patch: fix");
    let findings = vec![(warn("changes.no-upstream-notes"), FindingRoute::Agent)];
    let mut round = 0;
    let case = run(false, &own, None, job(findings), &mut move |ctx, _| {
        round += 1;
        let p = ctx.checkout_dir.join("a.patch");
        let text = std::fs::read_to_string(&p).unwrap();
        std::fs::write(&p, format!("Upstream: not yet reported\n{text}")).unwrap();
        let mut out = outcome(
            Action::Edited,
            vec![("changes.no-upstream-notes", FindingStatus::Fixed)],
        );
        out.changelog = "Add the Upstream header to a.patch".into();
        out.evidence = vec!["Read a.patch".into()];
        out.changes_entry = if round == 1 {
            "- Update to 1.0".into()
        } else {
            "- Update to 1.0\n- Add a.patch: fix\n- Add the Upstream header to a.patch\n  * https://x.test/notes".into()
        };
        out
    });
    assert!(case.result.is_ok(), "{:?}", case.result);
    assert_eq!(case.contexts.len(), 2);
    assert!(
        case.contexts[1].contains("changes_entry drops the bullet"),
        "{}",
        case.contexts[1]
    );
    assert!(
        case.changes.contains("  * https://x.test/notes"),
        "{}",
        case.changes
    );
    // The rolled-back first edit left no second header line.
    let patch = std::fs::read_to_string(case.checkout.join("a.patch")).unwrap();
    assert_eq!(patch.matches("Upstream: not yet reported").count(), 1);
}

#[test]
fn without_the_agent_an_agent_finding_is_not_fixed() {
    let findings = vec![(warn("patch.header"), FindingRoute::Agent)];
    let case = run(
        true,
        &own_entry("- Update to 1.0"),
        None,
        job(findings),
        &mut |_, _| panic!("no turn"),
    );
    assert_eq!(outcomes(&case), [("patch.header", "agent", "not_fixed")]);
    assert!(case.events[0].reason.contains("--no-agent"));
    assert_eq!(*failed(&case).0, FailureKind::NoFix);
}

#[test]
fn a_stage_that_settled_in_an_earlier_run_does_not_run_again() {
    let end = crate::bugfix::FixEnd {
        nonce: NONCE.into(),
        verdict: "fixed".into(),
        patch: None,
        applied: None,
        reason: String::new(),
    };
    let findings = vec![(warn("patch.header"), FindingRoute::Agent)];
    let case = run(
        false,
        &own_entry("- a"),
        Some(end),
        job(findings),
        &mut |_, _| panic!("no turn"),
    );
    assert!(case.result.is_ok());
    assert!(case.events.is_empty());
}
