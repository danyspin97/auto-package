//! Tests of the review records: a crash between an agent's write and its review,
//! a record that cannot be written, hashes of raw bytes, and declared patches
//! with any extension in the snapshot.

use super::*;

const SPEC: &str =
    "Name: foo\nVersion: 1.0\nPatch0: a.patch\nPatch1: b.mbox\n\n%prep\n%autosetup -p1\n";

/// A checkout with `SPEC`, `a.patch` and `b.mbox`, and a context over it.
pub(super) fn with_ctx<R>(f: impl FnOnce(&RunCtx, &Path) -> R) -> R {
    let root = tempfile::tempdir().unwrap();
    let work = root.path().join("work");
    let state_root = work.join("state");
    let mut config = super::hygiene_tests::config("foo", &state_root);
    config.rpm_name = "foo".into();
    config.work_dir = work.clone();
    let ws = Workspace::open(&state_root, "foo").unwrap();
    let checkout = work.join("proj/foo");
    std::fs::create_dir_all(&checkout).unwrap();
    std::fs::write(checkout.join("foo.spec"), SPEC).unwrap();
    std::fs::write(checkout.join("a.patch"), "one\n").unwrap();
    std::fs::write(checkout.join("b.mbox"), b"two\xff\n").unwrap();
    let run_dir = ap_core::run::RunDir::allocate(&work).unwrap();
    let pkg_run = run_dir.pkg("foo");
    let ctx = RunCtx {
        config: &config,
        pkg_run: &pkg_run,
        reporter: &ap_core::sink::NULL_REPORTER,
        ws,
        rpm_name: "foo".into(),
        obs_name: "foo".into(),
        checkout_dir: checkout.clone(),
        spec_path: checkout.join("foo.spec"),
        signals: Default::default(),
    };
    f(&ctx, &checkout)
}

fn kind(result: Option<PipelineResult>) -> Option<FailureKind> {
    match result {
        Some(PipelineResult::Failed { kind, .. }) => Some(kind),
        Some(other) => panic!("{other:?}"),
        None => None,
    }
}

#[test]
fn a_crash_after_the_agent_wrote_is_found_on_the_rerun() {
    with_ctx(|ctx, co| {
        // The turn began and recorded the patches; then the process died.
        begin_turn(&ctx.ws, &ctx.spec_path).unwrap();
        assert_eq!(kind(refuse_unreviewed(ctx)), None, "nothing changed yet");
        begin_turn(&ctx.ws, &ctx.spec_path).unwrap();
        std::fs::write(co.join("a.patch"), "rewritten\n").unwrap();
        assert_eq!(
            kind(refuse_unreviewed(ctx)),
            Some(FailureKind::NeedsMaintainer)
        );
        // The copies of the patches as they were stay for the maintainer.
        let copy = ctx.ws.dir().join("patch-review/start/a.patch");
        assert_eq!(std::fs::read_to_string(copy).unwrap(), "one\n");
        // Put back by hand: settled, and the record goes.
        std::fs::write(co.join("a.patch"), "one\n").unwrap();
        assert_eq!(kind(refuse_unreviewed(ctx)), None);
        assert!(!ctx.ws.dir().join("patch-review/in-progress").exists());
    });
}

#[test]
fn a_new_patch_file_after_a_crash_is_found_too() {
    with_ctx(|ctx, co| {
        begin_turn(&ctx.ws, &ctx.spec_path).unwrap();
        let spec = SPEC.replace("Patch1: b.mbox", "Patch1: b.mbox\nPatch2: c.patch");
        std::fs::write(&ctx.spec_path, spec).unwrap();
        std::fs::write(co.join("c.patch"), "new\n").unwrap();
        assert_eq!(
            kind(refuse_unreviewed(ctx)),
            Some(FailureKind::NeedsMaintainer)
        );
    });
}

#[test]
fn a_record_that_cannot_be_written_fails_the_settling_closed() {
    with_ctx(|ctx, co| {
        // A new patch the stage never reviewed, and both places for its record
        // are blocked: a directory where each file would go.
        let spec = SPEC.replace("Patch1: b.mbox", "Patch1: b.mbox\nPatch2: c.patch");
        std::fs::write(&ctx.spec_path, spec).unwrap();
        std::fs::write(co.join("c.patch"), "new\n").unwrap();
        let primary = ctx.ws.dir().join("patch-review/pending");
        std::fs::create_dir_all(&primary).unwrap();
        let dir = ctx.ws.dir();
        let name = dir.file_name().unwrap().to_string_lossy().into_owned();
        std::fs::create_dir_all(
            dir.parent()
                .unwrap()
                .join(format!("{name}-patch-review-pending")),
        )
        .unwrap();
        let start = std::collections::BTreeMap::from([
            ("a.patch".to_string(), Some(b"one\n".to_vec())),
            ("b.mbox".to_string(), Some(b"two\xff\n".to_vec())),
        ]);
        let settled = settle_unreviewed(ctx, &start, &Default::default());
        assert!(settled.is_err(), "{settled:?}");
        let stop = settle_stop(ctx, &settled, ctx.failed("x".into()));
        assert_eq!(kind(Some(stop)), Some(FailureKind::NeedsMaintainer));
    });
}

#[test]
fn a_record_falls_back_when_the_workspace_will_not_take_it() {
    with_ctx(|ctx, co| {
        let blocked = ctx.ws.dir().join("patch-review");
        std::fs::write(&blocked, "a file where the directory goes").unwrap();
        mark_reviewer_rewrite(&ctx.ws, co, "a.patch").unwrap();
        assert_eq!(reviewer_rewrites(&ctx.ws, co), ["a.patch"]);
    });
}

#[test]
fn the_marks_are_bound_to_the_raw_bytes() {
    with_ctx(|ctx, co| {
        mark_reviewer_rewrite(&ctx.ws, co, "b.mbox").unwrap();
        assert_eq!(reviewer_rewrites(&ctx.ws, co), ["b.mbox"]);
        // A change inside invalid UTF-8 is a change.
        std::fs::write(co.join("b.mbox"), b"two\xfe\n").unwrap();
        assert!(reviewer_rewrites(&ctx.ws, co).is_empty());
        assert!(was_reviewer_rewrite(&ctx.ws, co, "b.mbox"));
        assert!(!was_reviewer_rewrite(&ctx.ws, co, "a.patch"));
    });
}

#[test]
fn a_declared_patch_with_any_extension_is_in_the_snapshot() {
    with_ctx(|_, co| {
        let snapshot = CheckoutSnapshot::take(co);
        assert_eq!(
            snapshot.content_of("b.mbox").as_deref(),
            Some(&b"two\xff\n"[..])
        );
        // A file no spec declares keeps no contents.
        std::fs::write(co.join("notes.txt"), "x").unwrap();
        assert_eq!(CheckoutSnapshot::take(co).content_of("notes.txt"), None);
    });
}

fn record_exists(ctx: &RunCtx) -> bool {
    ctx.ws.dir().join("patch-review/in-progress").exists()
}

#[test]
fn a_turn_leaves_a_record_only_when_it_changed_a_patch() {
    with_ctx(|ctx, co| {
        drop(TurnRecord::begin(&ctx.ws, &ctx.spec_path).unwrap());
        assert!(!record_exists(ctx), "no patch changed: no record");
        {
            let _turn = TurnRecord::begin(&ctx.ws, &ctx.spec_path).unwrap();
            assert!(record_exists(ctx));
            std::fs::write(co.join("a.patch"), "rewritten\n").unwrap();
        }
        assert!(record_exists(ctx), "a patch differs: the record stays");
        // The next turn keeps the oldest bytes, not the rewrite.
        drop(TurnRecord::begin(&ctx.ws, &ctx.spec_path).unwrap());
        let copy = ctx.ws.dir().join("patch-review/start/a.patch");
        assert_eq!(std::fs::read_to_string(copy).unwrap(), "one\n");
    });
}

#[test]
fn the_run_start_drops_an_equal_record_and_keeps_a_different_one() {
    with_ctx(|ctx, co| {
        begin_turn(&ctx.ws, &ctx.spec_path).unwrap();
        assert!(settle_record(&ctx.ws, &ctx.spec_path));
        assert!(!record_exists(ctx));
        begin_turn(&ctx.ws, &ctx.spec_path).unwrap();
        // A carry-forward, a maintainer or an agent changed a patch between runs.
        std::fs::write(co.join("a.patch"), "changed\n").unwrap();
        assert!(!settle_record(&ctx.ws, &ctx.spec_path));
        assert!(record_exists(ctx));
        assert_eq!(
            kind(refuse_unreviewed(ctx)),
            Some(FailureKind::NeedsMaintainer)
        );
    });
}

#[test]
fn a_package_that_gains_its_first_patch_is_not_refused() {
    with_ctx(|ctx, co| {
        std::fs::write(
            &ctx.spec_path,
            "Name: foo\nVersion: 1.0\n\n%prep\n%autosetup -p1\n",
        )
        .unwrap();
        begin_turn(&ctx.ws, &ctx.spec_path).unwrap();
        std::fs::write(&ctx.spec_path, SPEC).unwrap();
        std::fs::write(co.join("a.patch"), "added by a maintainer\n").unwrap();
        assert!(settle_record(&ctx.ws, &ctx.spec_path));
        assert_eq!(kind(refuse_unreviewed(ctx)), None);
    });
}

#[test]
fn another_specs_record_is_never_read() {
    with_ctx(|ctx, co| {
        begin_turn(&ctx.ws, &ctx.spec_path).unwrap();
        std::fs::write(co.join("a.patch"), "changed\n").unwrap();
        let other = co.join("bar.spec");
        std::fs::write(&other, SPEC).unwrap();
        assert!(settle_record(&ctx.ws, &other), "not this spec's record");
        assert!(record_exists(ctx), "and left alone");
    });
}

#[test]
fn a_turn_that_cannot_be_recorded_does_not_start() {
    with_ctx(|ctx, _| {
        // A file where the workspace's patch-review directory goes.
        std::fs::write(ctx.ws.dir().join("patch-review"), "x").unwrap();
        let err = TurnRecord::begin(&ctx.ws, &ctx.spec_path)
            .err()
            .expect("must fail");
        assert!(err.contains("cannot record the patches"), "{err}");
    });
}

#[test]
fn the_messages_name_absolute_paths_and_the_records_are_private() {
    with_ctx(|ctx, co| {
        std::fs::write(
            &ctx.spec_path,
            SPEC.replace("Patch1: b.mbox\n", "Patch1: b.mbox\nPatch2: c.patch\n"),
        )
        .unwrap();
        std::fs::write(co.join("c.patch"), "new\n").unwrap();
        let start = std::collections::BTreeMap::from([
            ("a.patch".to_string(), Some(b"one\n".to_vec())),
            ("b.mbox".to_string(), Some(b"two\xff\n".to_vec())),
        ]);
        settle_unreviewed(ctx, &start, &Default::default()).unwrap();
        let Some(PipelineResult::Failed { reason, .. }) = refuse_unreviewed(ctx) else {
            panic!("a new unreviewed patch is refused");
        };
        assert!(
            reason.contains(
                &ctx.ws
                    .dir()
                    .join("patch-review/pending")
                    .display()
                    .to_string()
            ),
            "{reason}"
        );
        assert!(
            reason.contains("-patch-review-pending"),
            "the fallback file too: {reason}"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(ctx.ws.dir().join("patch-review/pending"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o077, 0, "{mode:o}");
        }
    });
}
