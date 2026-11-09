//! The precheck that runs before a try's review turn: a package waiting on
//! names that are not built yet ends `MissingDeps` there, and a record whose
//! names are RPMs now is discarded so the try builds.

use super::wait_patience::MAX_CONSULTATIONS;
use super::wait_record::*;
use crate::pipeline::hygiene_tests::config;
use crate::pipeline::{PipelineResult, RunCtx};
use crate::workspace::Workspace;
use ap_core::event::Event;
use ap_core::sink::Reporter;

/// The events of a precheck: the progress lines it reports.
#[derive(Default)]
struct Progress(std::sync::Mutex<Vec<String>>);

impl Reporter for Progress {
    fn report(&self, e: &Event) {
        if let Event::Progress { message, .. } = e {
            self.0.lock().unwrap().push(message.clone());
        }
    }
}

const SPEC: &str = "Name: foo\nVersion: 1.0\nRelease: 0\nSummary: x\n";

/// A work directory with the package's checkout (the spec on disk) and an
/// RPM directory, and a run context over them.
fn with_ctx<T>(
    spec: &str,
    reporter: &Progress,
    f: impl FnOnce(&RunCtx, &std::path::Path) -> T,
) -> T {
    let tmp = tempfile::tempdir().unwrap();
    let work = tmp.path().to_path_buf();
    let state_root = work.join("state");
    let mut config = config("foo", &state_root);
    config.rpm_name = "foo".into();
    config.work_dir = work.clone();
    config.rpms_dir = work.join("rpms");
    std::fs::create_dir_all(&config.rpms_dir).unwrap();
    let checkout = work.join("proj/foo");
    std::fs::create_dir_all(&checkout).unwrap();
    let spec_path = checkout.join("foo.spec");
    std::fs::write(&spec_path, spec).unwrap();
    let ws = Workspace::open(&state_root, "foo").unwrap();
    let run = ap_core::run::RunDir::allocate(&work).unwrap();
    let pkg_run = run.pkg("foo");
    let ctx = RunCtx {
        config: &config,
        pkg_run: &pkg_run,
        reporter,
        ws,
        rpm_name: "foo".into(),
        obs_name: "foo".into(),
        checkout_dir: checkout.clone(),
        spec_path,
        signals: Default::default(),
    };
    f(&ctx, &work)
}

/// The record a round leaves when it ends waiting on `foo`, for `spec` as on disk.
fn save_waiting(ctx: &RunCtx, spec: &str) {
    std::fs::write(ctx.spec_path.clone(), spec).unwrap();
    save_missing(ctx, &["foo".to_string()], &[], &[]);
}

#[test]
fn a_waiting_package_ends_before_any_turn_when_nothing_is_built() {
    let reporter = Progress::default();
    with_ctx(SPEC, &reporter, |ctx, _| {
        save_waiting(ctx, SPEC);
        let result = precheck(ctx);
        let Some(PipelineResult::MissingDeps { deps, .. }) = result else {
            panic!("expected MissingDeps, got {result:?}");
        };
        assert_eq!(deps, vec!["foo".to_string()]);
        // Only the progress line: no build step, no turn, no commit was reported.
        assert_eq!(reporter.0.lock().unwrap().len(), 1);
        // The record stays: the same answer holds for the next try.
        assert!(ctx.ws.dir().join("unresolvable-wait.json").exists());
    });
}

#[test]
fn a_built_dependency_discards_the_record_so_the_try_builds() {
    let reporter = Progress::default();
    with_ctx(SPEC, &reporter, |ctx, work| {
        save_waiting(ctx, SPEC);
        std::fs::write(work.join("rpms/python313-foo-1.0-1.1.noarch.rpm"), b"").unwrap();
        assert!(precheck(ctx).is_none());
        assert!(!ctx.ws.dir().join("unresolvable-wait.json").exists());
        assert!(reporter.0.lock().unwrap().is_empty());
    });
}

#[test]
fn an_edited_spec_is_not_waited_on() {
    let reporter = Progress::default();
    with_ctx(SPEC, &reporter, |ctx, _| {
        save_waiting(ctx, SPEC);
        std::fs::write(&ctx.spec_path, "Name: foo\nVersion: 1.1\n").unwrap();
        assert!(precheck(ctx).is_none());
        assert!(ctx.ws.dir().join("unresolvable-wait.json").exists());
    });
}

/// The spec asks `foo >= 2.0`: the record keeps that bound, so an RPM of foo
/// at 2.0 here is not enough while the repository's copy is older.
#[test]
fn a_spec_bound_is_kept_and_checked_against_the_repository_copy() {
    let reporter = Progress::default();
    let spec = "Name: bar\nVersion: 1\nRequires: python313-foo >= 2.0\n";
    with_ctx(spec, &reporter, |ctx, work| {
        save_missing(ctx, &["foo".to_string()], &[], &[]);
        std::fs::write(work.join("rpms/python313-foo-2.0-1.1.noarch.rpm"), b"").unwrap();
        let older = |_: &str, _: &str| false;
        assert!(matches!(
            precheck_with(ctx, &older, &|_| None),
            Some(PipelineResult::MissingDeps { .. })
        ));
        assert!(ctx.ws.dir().join("unresolvable-wait.json").exists());
        assert!(precheck_with(ctx, &|_, _| true, &|_| None).is_none());
        assert!(!ctx.ws.dir().join("unresolvable-wait.json").exists());
    });
}

/// A wait that never progresses ends as `NeedsMaintainer` on its last
/// consultation, once, and its record is removed.
#[test]
fn a_wait_that_never_progresses_ends_as_needs_a_human() {
    let reporter = Progress::default();
    with_ctx(SPEC, &reporter, |ctx, _| {
        save_waiting(ctx, SPEC);
        let nothing_built = |_: &str, _: &str| true;
        for _ in 1..MAX_CONSULTATIONS {
            assert!(matches!(
                precheck_with(ctx, &nothing_built, &|_| None),
                Some(PipelineResult::MissingDeps { .. })
            ));
        }
        let Some(PipelineResult::Failed { kind, reason, .. }) =
            precheck_with(ctx, &nothing_built, &|_| None)
        else {
            panic!("expected a failure at the last consultation");
        };
        assert!(matches!(
            kind,
            crate::pipeline::FailureKind::NeedsMaintainer
        ));
        assert!(
            reason.starts_with(&format!(
                "needs a human: waited {MAX_CONSULTATIONS} times for foo, which is not built here"
            )),
            "{reason}"
        );
        assert!(!ctx.ws.dir().join("unresolvable-wait.json").exists());
    });
}

/// A round that waits on something else (progress) starts the count again.
#[test]
fn progress_restarts_the_count_of_a_wait() {
    let reporter = Progress::default();
    with_ctx(SPEC, &reporter, |ctx, _| {
        save_waiting(ctx, SPEC);
        for _ in 0..5 {
            precheck_with(ctx, &|_, _| true, &|_| None);
        }
        let read = || -> serde_json::Value {
            let text =
                std::fs::read_to_string(ctx.ws.dir().join("unresolvable-wait.json")).unwrap();
            serde_json::from_str(&text).unwrap()
        };
        assert_eq!(read()["consulted"], 5);
        save_missing(ctx, &["foo".to_string()], &["libbar".to_string()], &[]);
        assert_eq!(read()["consulted"], 0);
    });
}
