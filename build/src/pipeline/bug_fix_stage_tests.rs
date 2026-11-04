use super::*;
use crate::backport::PreparedPatch;
use crate::bugfix::{FixJob, FixTask, FixedIn, Found, HeaderFacts, UpstreamStatus};
use crate::patch_check::{CheckReport, PatchResult, PatchStatus, Refresh};
use crate::pipeline::bug_fix_stage::*;

mod rounds;

const NAME: &str = "foo-CVE-2025-1234.patch";
/// Applies to the sources of `source_tarball`: `f.txt` is `a B c d`.
const FIX_DIFF: &str = "--- a/f.txt\n+++ b/f.txt\n@@ -1,4 +1,4 @@\n a\n B\n-c\n+C\n d\n";
const NONCE: &str = "nonce-1";

fn refs() -> Vec<String> {
    vec!["bsc#1234567".into(), "CVE-2025-1234".into()]
}

fn patch_bytes(diff: &str) -> Vec<u8> {
    crate::bugfix::with_fix_header(
        diff.as_bytes(),
        &HeaderFacts {
            refs: refs(),
            upstream: Some("https://github.com/o/r/commit/abc1234".into()),
            status: UpstreamStatus::Merged,
            reason: "check the length".into(),
        },
    )
}

fn prepared(name: &str, bytes: Vec<u8>) -> PreparedPatch {
    PreparedPatch {
        file_name: name.into(),
        bytes,
        origin: "https://github.com/o/r/commit/abc1234.patch".into(),
        commit: Some("abc1234abc1234abc1234abc1234abc1234abc12".into()),
        reason: "check the length".into(),
    }
}

fn job(patches: Vec<PreparedPatch>) -> FixJob {
    FixJob {
        pkg: "foo".into(),
        refs: refs(),
        description: None,
        bug_texts: Vec::new(),
        read: vec!["bugzilla.suse.com bsc#1234567".into()],
        not_read: Vec::new(),
        warnings: Vec::new(),
        upstream_repo: Some("https://github.com/o/r".into()),
        patches,
        candidates: Vec::new(),
        unmerged: Vec::new(),
        found: Found::NotFound,
        fixed_in: FixedIn::NoneYet,
        fetch_hosts: Vec::new(),
        nonce: NONCE.into(),
    }
}

fn good_job() -> FixJob {
    job(vec![prepared(NAME, patch_bytes(FIX_DIFF))])
}

fn quilt_missing() -> bool {
    let missing = std::process::Command::new("quilt")
        .arg("--version")
        .env("QUILTRC", "/dev/null")
        .output()
        .map_or(true, |o| !o.status.success());
    if missing {
        eprintln!("quilt is not installed; skipping");
    }
    missing
}

/// The events of a stage.
#[derive(Default)]
struct Log(std::sync::Mutex<Vec<ap_core::fix_event::BugFixEvent>>);

impl Reporter for Log {
    fn report(&self, e: &Event) {
        if let Event::BugFix(b) = e {
            self.0.lock().unwrap().push(b.clone());
        }
    }
}

type Check = dyn FnMut(&RunCtx, Refresh) -> CheckReport;
type Turn = dyn FnMut(&RunCtx, &str) -> Result<crate::agent::TurnOutcome, (String, FailureKind)>;
type Review = dyn FnMut(&RunCtx, &str) -> Result<crate::agent::TurnOutcome, (String, FailureKind)>;

/// A check that gives every declared patch `status`; fuzzy ones refresh to `refreshed`.
fn check_all(status: PatchStatus, refreshed: Option<Vec<u8>>) -> Box<Check> {
    Box::new(move |ctx, refresh| {
        let spec = std::fs::read_to_string(&ctx.spec_path).unwrap();
        CheckReport {
            patches: crate::patch_check::declared_patches(&spec)
                .into_iter()
                .map(|(_, name)| PatchResult {
                    name,
                    level: 1,
                    status: status.clone(),
                    refreshed: (refresh == Refresh::Fuzz)
                        .then(|| refreshed.clone())
                        .flatten(),
                })
                .collect(),
            not_run: None,
        }
    })
}

fn accept() -> Box<Review> {
    Box::new(|_, _| {
        let mut out = pipeline_wheel_outcome("x");
        out.action = crate::agent::Action::NoChange;
        out.patch_review = Some(crate::agent::PatchReviewAnswer {
            verdict: crate::agent::PatchReviewVerdict::Accept,
            reason: "the upstream commit, as it is".into(),
            problems: Vec::new(),
        });
        Ok(out)
    })
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

struct Case {
    _root: tempfile::TempDir,
    result: Result<(), PipelineResult>,
    events: Vec<ap_core::fix_event::BugFixEvent>,
    reviews: Vec<String>,
    /// The context of each fix turn.
    contexts: Vec<String>,
    spec: String,
    changes: String,
    checkout: PathBuf,
    ws_dir: PathBuf,
    notes: String,
}

struct Rig {
    no_agent: bool,
    spec: String,
    spent: f64,
    tracked: Option<Vec<&'static str>>,
    /// A file `NAME` the package has already.
    existing: Option<String>,
    prior_end: Option<crate::bugfix::FixEnd>,
}

impl Rig {
    fn new() -> Rig {
        Rig {
            no_agent: false,
            spec: "Name: foo\nVersion: 1.0\nRelease: 0\nSummary: test\nLicense: MIT\nSource0: foo-1.0.tar.gz\n\n%prep\n%autosetup -p1\n".into(),
            spent: 0.0,
            tracked: None,
            existing: None,
            prior_end: None,
        }
    }

    fn run(&self, job: FixJob, check: &mut Check, review: &mut Review) -> Case {
        self.run_turns(job, check, &mut |_, _| panic!("no fix turn"), review)
    }

    fn run_turns(
        &self,
        job: FixJob,
        check: &mut Check,
        fix_turn: &mut Turn,
        review: &mut Review,
    ) -> Case {
        let root = tempfile::tempdir().unwrap();
        let work = root.path().join("work");
        let state_root = work.join("state");
        let mut config = super::hygiene_tests::config("foo", &state_root);
        config.rpm_name = "foo".into();
        config.work_dir = work.clone();
        config.project = "proj".into();
        config.no_agent = self.no_agent;
        config.budget_usd = 100.0;
        config.max_attempts = 10;
        config.version = "1.0".into();
        let ws = Workspace::open(&state_root, "foo").unwrap();
        let checkout = work.join("proj/foo");
        std::fs::create_dir_all(checkout.join(".osc")).unwrap();
        source_tarball(&checkout);
        if let Some(names) = &self.tracked {
            let entries: String = names
                .iter()
                .map(|n| format!("<entry name=\"{n}\" md5=\"x\" size=\"1\" mtime=\"1\" />"))
                .collect();
            std::fs::write(
                checkout.join(".osc/_files"),
                format!("<directory>{entries}</directory>"),
            )
            .unwrap();
        }
        if let Some(body) = &self.existing {
            std::fs::write(checkout.join(NAME), body).unwrap();
        }
        let spec_path = checkout.join("foo.spec");
        std::fs::write(&spec_path, &self.spec).unwrap();
        let sep = "-".repeat(67);
        let changes = format!("{sep}\nMon Jan  1 00:00:00 UTC 2026 - x@y\n\n- Update to 1.0\n\n");
        std::fs::write(checkout.join("foo.changes"), &changes).unwrap();
        if let Some(end) = &self.prior_end {
            crate::bugfix::write_fix_end(&ws.dir(), end).unwrap();
        }
        let run = ap_core::run::RunDir::allocate(&work).unwrap();
        let pkg_run = run.pkg("foo");
        let log = Log::default();
        let ctx = RunCtx {
            config: &config,
            pkg_run: &pkg_run,
            reporter: &log,
            ws,
            rpm_name: "foo".into(),
            obs_name: "foo".into(),
            checkout_dir: checkout.clone(),
            spec_path: spec_path.clone(),
            signals: Default::default(),
        };
        let mut st = RunState {
            step_attempts: Default::default(),
            sdist_top_level: String::new(),
            from_repo_archive: false,
            attempts: 0,
            spent_usd: self.spent,
            session_id: None,
            last_log_digest: None,
            identical_logs: 0,
            same_shape_failures: 0,
            build_rounds: Default::default(),
            last_failure_shape: None,
            relaxed_from_spec: None,
            post_review_done: false,
            post_review_revert: None,
            rpmlint_shown: Default::default(),
            update: None,
        };
        let step_path = root.path().join("step");
        std::fs::create_dir_all(&step_path).unwrap();
        let mut reviews = Vec::new();
        let mut contexts: Vec<String> = Vec::new();
        let task = FixTask::Bug(job);
        // The new entry `osc vc` would write.
        let add = |dir: &Path, message: &str| {
            let file = dir.join("foo.changes");
            let old = std::fs::read_to_string(&file).unwrap();
            let sep = "-".repeat(67);
            let text = format!("{sep}\nTue Jan  2 00:00:00 UTC 2026 - x@y\n\n- {message}\n\n{old}");
            std::fs::write(&file, text).map_err(|e| e.to_string())
        };
        let mut run_turn = |ctx: &RunCtx, _: &mut RunState, _: usize, context: &str| {
            contexts.push(context.to_string());
            let out = fix_turn(ctx, context)?;
            Ok((
                StepDir {
                    path: step_path.clone(),
                    id: ap_core::event::StepId(2),
                },
                out,
            ))
        };
        let mut turn = |ctx: &RunCtx, _: &mut RunState, _: &str, context: &str| {
            reviews.push(context.to_string());
            let out = review(ctx, context)?;
            Ok((
                StepDir {
                    path: step_path.clone(),
                    id: ap_core::event::StepId(3),
                },
                out,
            ))
        };
        let result = bug_fix_stage_with(
            &ctx,
            &mut st,
            &task,
            FixSeams {
                check,
                turn: &mut run_turn,
                review: &mut turn,
                add_entry: &add,
            },
        );
        Case {
            _root: root,
            result,
            events: log.0.lock().unwrap().clone(),
            reviews,
            contexts,
            spec: std::fs::read_to_string(&spec_path).unwrap(),
            changes: std::fs::read_to_string(checkout.join("foo.changes")).unwrap(),
            notes: std::fs::read_to_string(ctx.ws.notes()).unwrap_or_default(),
            ws_dir: ctx.ws.dir().to_path_buf(),
            checkout,
        }
    }
}

fn failed(case: &Case) -> (&FailureKind, &str) {
    match &case.result {
        Err(PipelineResult::Failed { kind, reason, .. }) => (kind, reason),
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_staged_patch_that_applies_is_added_reviewed_and_named_in_the_changes() {
    let case = Rig::new().run(
        good_job(),
        &mut *check_all(PatchStatus::Applies, None),
        &mut *accept(),
    );
    assert!(case.result.is_ok(), "{:?}", case.result);
    assert!(
        case.spec.contains(&format!(
            "# PATCH-FIX-UPSTREAM {NAME} bsc#1234567 CVE-2025-1234 -- check the length\nPatch0:"
        )),
        "{}",
        case.spec
    );
    // Aligned with the Source: above it.
    assert!(
        case.spec.contains(&format!("Patch0:  {NAME}")),
        "{}",
        case.spec
    );
    assert!(case.checkout.join(NAME).exists());
    // One new entry, with the references.
    assert!(
        case.changes.contains(&format!(
            "- Add {NAME}: check the length (bsc#1234567, CVE-2025-1234)"
        )),
        "{}",
        case.changes
    );
    assert!(case.changes.contains("- Update to 1.0"));
    // The patch review saw an upstream commit staged by the tool.
    assert_eq!(case.reviews.len(), 1);
    assert!(
        case.reviews[0].contains("staged by the tool"),
        "{}",
        case.reviews[0]
    );
    // The record.
    let kinds: Vec<_> = case.events.iter().map(|e| e.stage).collect();
    assert_eq!(
        kinds,
        [
            ap_core::fix_event::BugFixStage::Input,
            ap_core::fix_event::BugFixStage::End
        ]
    );
    assert_eq!(case.events[0].patch.as_deref(), Some(NAME));
    assert_eq!(
        case.events[0].patch_sha256.as_ref().map(String::len),
        Some(64)
    );
    assert_eq!(case.events[1].verdict.as_deref(), Some("fixed"));
    assert_eq!(case.events[1].applied.as_deref(), Some("clean"));
    let end = crate::bugfix::read_fix_end(&case.ws_dir, NONCE).unwrap();
    assert_eq!(end.verdict, "fixed");
    assert_eq!(end.patch.as_deref(), Some(NAME));
    assert!(
        case.notes.contains("## Bug fix input") && case.notes.contains("## Bug fix: fixed (clean)")
    );
}

#[test]
fn without_the_agent_the_patch_is_added_with_no_review() {
    let mut rig = Rig::new();
    rig.no_agent = true;
    let case = rig.run(
        good_job(),
        &mut *check_all(PatchStatus::Applies, None),
        &mut |_, _| panic!("no review without the agent"),
    );
    assert!(case.result.is_ok(), "{:?}", case.result);
    assert!(case.reviews.is_empty());
}

#[test]
fn a_patch_with_fuzz_is_refreshed_and_the_refreshed_bytes_are_kept() {
    let refreshed = patch_bytes(FIX_DIFF);
    let mut job = good_job();
    job.patches[0].bytes =
        patch_bytes("--- a/f.txt\n+++ b/f.txt\n@@ -1,4 +1,4 @@\n a\n x\n-c\n+C\n d\n");
    let case = Rig::new().run(
        job,
        &mut *check_all(
            PatchStatus::AppliesWithOffsetOrFuzz {
                fuzz: true,
                output: "fuzz 1".into(),
            },
            Some(refreshed.clone()),
        ),
        &mut *accept(),
    );
    assert!(case.result.is_ok(), "{:?}", case.result);
    assert_eq!(std::fs::read(case.checkout.join(NAME)).unwrap(), refreshed);
    assert_eq!(case.events[1].applied.as_deref(), Some("refreshed"));
    assert!(case.reviews[0].contains("refreshed for this version"));
}

#[test]
fn a_patch_quilt_can_reverse_apply_means_the_bug_is_fixed_already() {
    let case = Rig::new().run(
        good_job(),
        &mut *check_all(
            PatchStatus::AlreadyApplied {
                output: "can be reverse-applied".into(),
            },
            None,
        ),
        &mut |_, _| panic!("no review"),
    );
    let (kind, reason) = failed(&case);
    assert_eq!(*kind, FailureKind::NoFix);
    assert!(reason.contains("reverse-applied"), "{reason}");
    assert_eq!(case.spec, Rig::new().spec, "the spec is put back");
    assert!(
        !case.checkout.join(NAME).exists(),
        "the staged file is gone"
    );
    assert_eq!(case.events[1].verdict.as_deref(), Some("already_fixed"));
    assert_eq!(
        crate::bugfix::read_fix_end(&case.ws_dir, NONCE)
            .unwrap()
            .verdict,
        "already_fixed"
    );
}

#[test]
fn a_rejected_review_needs_a_maintainer_and_puts_the_checkout_back() {
    let case = Rig::new().run(
        good_job(),
        &mut *check_all(PatchStatus::Applies, None),
        &mut |_, _| {
            let mut out = pipeline_wheel_outcome("x");
            out.action = crate::agent::Action::NoChange;
            out.patch_review = Some(crate::agent::PatchReviewAnswer {
                verdict: crate::agent::PatchReviewVerdict::Reject,
                reason: "not the fix".into(),
                problems: Vec::new(),
            });
            Ok(out)
        },
    );
    let (kind, reason) = failed(&case);
    assert_eq!(*kind, FailureKind::NeedsMaintainer);
    assert!(
        reason.contains("rejected") && reason.contains("not the fix"),
        "{reason}"
    );
    assert_eq!(case.spec, Rig::new().spec);
    assert!(!case.checkout.join(NAME).exists());
}

#[test]
fn a_different_file_of_that_name_in_the_package_needs_a_maintainer() {
    let mut rig = Rig::new();
    rig.tracked = Some(vec![NAME]);
    rig.existing = Some("something else".into());
    let case = rig.run(
        good_job(),
        &mut *check_all(PatchStatus::Applies, None),
        &mut *accept(),
    );
    let (kind, reason) = failed(&case);
    assert_eq!(*kind, FailureKind::NeedsMaintainer);
    assert!(reason.contains("differs"), "{reason}");
}

#[test]
fn a_fix_that_was_settled_by_this_run_is_not_done_again() {
    let mut rig = Rig::new();
    rig.spec.push_str(&format!(
        "# PATCH-FIX-UPSTREAM {NAME} bsc#1234567 CVE-2025-1234 -- x\nPatch0: {NAME}\n"
    ));
    rig.prior_end = Some(crate::bugfix::FixEnd {
        nonce: NONCE.into(),
        verdict: "fixed".into(),
        patch: Some(NAME.into()),
        applied: Some("clean".into()),
        reason: "done".into(),
    });
    let case = rig.run(good_job(), &mut |_, _| panic!("no check"), &mut |_, _| {
        panic!("no review")
    });
    assert!(case.result.is_ok());
    assert!(case.events.is_empty(), "no stage ran");
}

#[test]
fn a_fix_added_by_an_earlier_run_is_no_fix() {
    let mut rig = Rig::new();
    rig.spec.push_str(&format!("Patch0: {NAME}\n"));
    rig.existing = Some(String::from_utf8(patch_bytes(FIX_DIFF)).unwrap());
    rig.prior_end = Some(crate::bugfix::FixEnd {
        nonce: "an-older-run".into(),
        verdict: "fixed".into(),
        patch: Some(NAME.into()),
        applied: None,
        reason: String::new(),
    });
    let case = rig.run(good_job(), &mut |_, _| panic!("no check"), &mut |_, _| {
        panic!("no review")
    });
    let (kind, reason) = failed(&case);
    assert_eq!(*kind, FailureKind::NoFix);
    assert!(reason.contains("earlier run"), "{reason}");
}

#[test]
fn a_spec_that_needs_the_agent_for_its_patch_line_gives_the_turn_the_staged_patch() {
    let mut rig = Rig::new();
    rig.spec = "Name: foo\nVersion: 1.0\nSource0: foo-1.0.tar.gz\nPatch0: a.patch\n\n%prep\n%setup -q\n%patch -P 0 -p1\n".into();
    let case = rig.run_turns(
        good_job(),
        &mut *check_all(PatchStatus::Applies, None),
        &mut |_, _| {
            let mut out = pipeline_wheel_outcome("x");
            out.action = crate::agent::Action::Blocked { why: "stop".into() };
            Ok(out)
        },
        &mut *accept(),
    );
    assert_eq!(*failed(&case).0, FailureKind::NeedsMaintainer);
    assert_eq!(case.contexts.len(), 1);
    let context = &case.contexts[0];
    assert!(
        context.contains(&format!("Staged upstream patch: {NAME}\n")),
        "{context}"
    );
    assert!(context.contains("%patch"), "{context}");
}

#[test]
fn quilt_applies_a_real_upstream_mail_on_a_small_tarball() {
    if quilt_missing() {
        return;
    }
    let case = Rig::new().run(good_job(), &mut run_patch_check, &mut *accept());
    assert!(case.result.is_ok(), "{:?}", case.result);
    assert_eq!(case.events[1].applied.as_deref(), Some("clean"));
}

#[test]
fn quilt_reverse_applies_a_patch_whose_change_is_in_the_sources() {
    if quilt_missing() {
        return;
    }
    // `B` is already in f.txt: the reverse of this patch is what the sources have.
    let job = job(vec![prepared(
        NAME,
        patch_bytes("--- a/f.txt\n+++ b/f.txt\n@@ -1,4 +1,4 @@\n a\n-b\n+B\n c\n d\n"),
    )]);
    let case = Rig::new().run(job, &mut run_patch_check, &mut |_, _| panic!("no review"));
    let (kind, _) = failed(&case);
    assert_eq!(*kind, FailureKind::NoFix);
}

#[test]
fn another_patch_of_the_same_name_is_not_taken_for_this_fix() {
    let mut rig = Rig::new();
    rig.spec.push_str(&format!("Patch0: {NAME}\n"));
    // Declared and present, with other code (or none): a maintainer's, not ours.
    for body in ["--- a/x\n+++ b/x\n@@ -1 +1 @@\n-a\n+b\n", "not a patch"] {
        rig.existing = Some(body.into());
        let case = rig.run(good_job(), &mut |_, _| panic!("no check"), &mut |_, _| {
            panic!("no review")
        });
        let (kind, reason) = failed(&case);
        assert_eq!(*kind, FailureKind::NeedsMaintainer, "{reason}");
        assert!(reason.contains("not this fix's"), "{reason}");
    }
    // Declared but not in the checkout at all.
    rig.existing = None;
    let case = rig.run(good_job(), &mut |_, _| panic!("no check"), &mut |_, _| {
        panic!("no review")
    });
    assert_eq!(*failed(&case).0, FailureKind::NeedsMaintainer);
}
