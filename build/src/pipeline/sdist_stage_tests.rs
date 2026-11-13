use super::*;
use crate::pipeline::archive::find_sdist_top_level;
use crate::pipeline::sdist_stage::{
    UNPACKED_SOURCE_MISSING, fetch_sdist_stage, fetch_sdist_stage_with, unpacked_source_missing,
    unpacked_top_level,
};
use crate::pipeline::source_pass::{
    NoTarball, SourcePass, classify_no_tarball, pass_will_run, source_pass_needed,
};

/// Discards every event.
struct Quiet;

impl Reporter for Quiet {
    fn report(&self, _: &Event) {}
}

/// Keeps the steps that finished (with their outcome) and the warnings.
#[derive(Default)]
struct Log {
    kinds: std::sync::Mutex<Vec<(ap_core::event::StepId, StepKind)>>,
    finished: std::sync::Mutex<Vec<(StepKind, String)>>,
    warnings: std::sync::Mutex<Vec<String>>,
}

impl Log {
    /// The outcomes of the steps of `kind`, in order.
    fn outcomes(&self, kind: StepKind) -> Vec<String> {
        self.finished
            .lock()
            .unwrap()
            .iter()
            .filter(|(k, _)| *k == kind)
            .map(|(_, o)| o.clone())
            .collect()
    }
}

impl Reporter for Log {
    fn report(&self, e: &Event) {
        match e {
            Event::StepStarted { step, kind, .. } => {
                self.kinds.lock().unwrap().push((*step, *kind))
            }
            Event::StepFinished { step, outcome, .. } => {
                let kind = self
                    .kinds
                    .lock()
                    .unwrap()
                    .iter()
                    .find(|(s, _)| s == step)
                    .map(|(_, k)| *k);
                if let Some(kind) = kind {
                    self.finished
                        .lock()
                        .unwrap()
                        .push((kind, outcome.to_string()));
                }
            }
            Event::Warning { message, .. } => self.warnings.lock().unwrap().push(message.clone()),
            _ => {}
        }
    }
}

/// A `.tar.zst` at `path` holding one small file for each of `entries`.
fn zst_tarball(path: &Path, entries: &[&str]) {
    let file = std::fs::File::create(path).unwrap();
    let enc = zstd::stream::write::Encoder::new(file, 0).unwrap();
    let mut ar = tar::Builder::new(enc);
    for name in entries {
        let body = b"x\n";
        let mut h = tar::Header::new_gnu();
        h.set_size(body.len() as u64);
        h.set_mode(0o644);
        h.set_cksum();
        ar.append_data(&mut h, name, &body[..]).unwrap();
    }
    ar.into_inner().unwrap().finish().unwrap();
}

/// A `.tar.gz` at `path` holding one small file for each of `entries`.
fn tarball(path: &Path, entries: &[&str]) {
    let file = std::fs::File::create(path).unwrap();
    let enc = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
    let mut ar = tar::Builder::new(enc);
    for name in entries {
        let body = b"x\n";
        let mut h = tar::Header::new_gnu();
        h.set_size(body.len() as u64);
        h.set_mode(0o644);
        h.set_cksum();
        ar.append_data(&mut h, name, &body[..]).unwrap();
    }
    ar.into_inner().unwrap().finish().unwrap();
}

#[test]
fn the_archive_directory_names_the_top_level_not_the_file() {
    let dir = tempfile::tempdir().unwrap();
    tarball(
        &dir.path().join("AMQPStorm-1.0.tar.gz"),
        &["amqpstorm-1.0/x"],
    );
    assert_eq!(
        find_sdist_top_level(dir.path(), "amqpstorm", "1.0"),
        "amqpstorm-1.0"
    );
}

#[test]
fn an_unreadable_tarball_falls_back_to_the_file_stem() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("AMQPStorm-1.0.tar.gz"), b"not gzip").unwrap();
    assert_eq!(
        find_sdist_top_level(dir.path(), "amqpstorm", "1.0"),
        "AMQPStorm-1.0"
    );
}

#[test]
fn the_unpacked_directory_replaces_a_stale_name() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("amqpstorm-1.0")).unwrap();
    assert_eq!(
        unpacked_top_level(dir.path(), "AMQPStorm-1.0"),
        "amqpstorm-1.0"
    );
}

#[test]
fn a_name_that_exists_is_kept() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("pkg-1.0")).unwrap();
    std::fs::create_dir_all(dir.path().join("other")).unwrap();
    assert_eq!(unpacked_top_level(dir.path(), "pkg-1.0"), "pkg-1.0");
}

#[test]
fn several_entries_or_a_file_keep_the_name() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("a")).unwrap();
    std::fs::create_dir_all(dir.path().join("b")).unwrap();
    assert_eq!(unpacked_top_level(dir.path(), "pkg-1.0"), "pkg-1.0");

    let file = tempfile::tempdir().unwrap();
    std::fs::write(file.path().join("pkg.py"), "").unwrap();
    assert_eq!(unpacked_top_level(file.path(), "pkg-1.0"), "pkg-1.0");
}

#[test]
fn an_empty_name_is_a_flat_unpack_and_stays_empty() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("only")).unwrap();
    assert_eq!(unpacked_top_level(dir.path(), ""), "");
}

/// Runs `f` on a checkout whose `AMQPStorm-1.0.tar.gz` holds `entries` (no
/// tarball for `None`), with `_service` set to `service` when it is not empty,
/// and the top-level name `hint`. The run's directories live for the call.
fn with_checkout<R>(
    entries: Option<&[&str]>,
    hint: &str,
    service: &str,
    f: impl FnOnce(&RunCtx, &mut RunState) -> R,
) -> R {
    with_checkout_as(&Quiet, "1.0", entries, hint, service, f)
}

/// [`with_checkout`] for a run of `version` that reports to `reporter`. The
/// tarball, when given, is `AMQPStorm-1.0.tar.gz` whatever the version.
pub(super) fn with_checkout_as<R>(
    reporter: &dyn Reporter,
    version: &str,
    entries: Option<&[&str]>,
    hint: &str,
    service: &str,
    f: impl FnOnce(&RunCtx, &mut RunState) -> R,
) -> R {
    let root = tempfile::tempdir().unwrap();
    let work = root.path().join("work");
    let state_root = work.join("state");
    let mut config = super::hygiene_tests::config("foo", &state_root);
    config.work_dir = work.clone();
    config.version = version.into();
    let checkout = work.join("p/foo");
    std::fs::create_dir_all(&checkout).unwrap();
    if let Some(entries) = entries {
        tarball(&checkout.join("AMQPStorm-1.0.tar.gz"), entries);
    }
    if !service.is_empty() {
        std::fs::write(checkout.join("_service"), service).unwrap();
    }
    let spec_path = checkout.join("foo.spec");
    std::fs::write(&spec_path, "Name: foo\nVersion: 1.0\n").unwrap();
    let run = ap_core::run::RunDir::allocate(&work).unwrap();
    let pkg_run = run.pkg("foo");
    let ws = Workspace::open(&state_root, "foo").unwrap();
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
    let mut st = RunState {
        step_attempts: Default::default(),
        sdist_top_level: hint.into(),
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
    f(&ctx, &mut st)
}

#[test]
fn the_source_link_follows_the_archive_directory() {
    with_checkout(
        Some(&["amqpstorm-1.0/x"]),
        "AMQPStorm-1.0",
        "",
        |ctx, st| {
            fetch_sdist_stage(ctx, st, false).unwrap_or_else(|_| panic!("the stage fails"));
            assert_eq!(st.sdist_top_level, "amqpstorm-1.0");
            let link = ctx.ws.dir().join("source");
            assert!(link.join("x").exists(), "source/ links the unpacked tree");
        },
    );
}

#[test]
fn a_tarball_with_several_top_entries_gets_no_link_to_a_guess() {
    with_checkout(
        Some(&["amqpstorm-1.0/x", "docs/y"]),
        "AMQPStorm-1.0",
        "",
        |ctx, st| {
            // No link is made, and the stage says so once instead of letting
            // every patch round fail on it.
            match fetch_sdist_stage(ctx, st, false) {
                Err(PipelineResult::Failed { reason, .. }) => {
                    assert_eq!(reason, UNPACKED_SOURCE_MISSING);
                }
                _ => panic!("the stage must stop"),
            }
            assert_eq!(st.sdist_top_level, "AMQPStorm-1.0");
            assert!(ctx.ws.dir().join("source").symlink_metadata().is_err());
        },
    );
}

#[test]
fn a_tarball_without_its_link_is_a_pipeline_fault() {
    with_checkout(
        Some(&["amqpstorm-1.0/x"]),
        "AMQPStorm-1.0",
        "",
        |ctx, st| {
            assert!(unpacked_source_missing(ctx), "no source/ yet");
            fetch_sdist_stage(ctx, st, false).unwrap_or_else(|_| panic!("the stage fails"));
            assert!(!unpacked_source_missing(ctx), "the stage made the link");
        },
    );
}

#[test]
fn a_source_the_pipeline_does_not_unpack_is_not_a_fault() {
    // No tarball at all.
    with_checkout(None, "AMQPStorm-1.0", "", |ctx, _| {
        assert!(!unpacked_source_missing(ctx));
    });
    // A source OBS makes at build time has no tarball to unpack.
    let service = "<services><service name=\"obs_scm\" mode=\"buildtime\"/></services>\n";
    with_checkout(
        Some(&["amqpstorm-1.0/x"]),
        "AMQPStorm-1.0",
        service,
        |ctx, _| {
            assert!(!unpacked_source_missing(ctx));
        },
    );
}

#[test]
fn the_pass_is_needed_only_for_a_service_that_makes_the_source() {
    let fetched = |name: &str, params: &str| {
        format!(
            "<services><service name=\"{name}\" mode=\"trylocal\">{params}</service></services>"
        )
    };
    let url = "<param name=\"url\">https://example.org/jay.git</param>";
    // A service that fetches the source, with and without a tarball in the checkout.
    assert!(source_pass_needed(&fetched("tar_scm", url), false, false));
    assert!(source_pass_needed(
        &fetched("cargo_vendor", url),
        false,
        false
    ));
    assert!(!source_pass_needed(
        &fetched("cargo_vendor", ""),
        false,
        false
    ));
    assert!(!source_pass_needed(&fetched("tar_scm", url), true, false));
    // OBS makes an obs_scm source at build time: nothing runs here.
    assert!(!source_pass_needed(&fetched("obs_scm", url), false, false));
    // A redistributed archive is never unpacked, and a checkout without a
    // `_service` has nothing to run.
    assert!(!source_pass_needed(&fetched("tar_scm", url), false, true));
    assert!(!source_pass_needed("", false, false));
    // download_files lists the sources to fetch; it is a pass too.
    assert!(source_pass_needed(
        &fetched("download_files", ""),
        false,
        false
    ));
    assert!(!source_pass_needed(
        &format!(
            "{}{}",
            fetched("obs_scm", url),
            fetched("download_files", "")
        ),
        false,
        false
    ));
}

#[test]
fn a_pass_that_makes_the_tarball_unpacks_it() {
    let log = Log::default();
    let service = "<services><service name=\"tar_scm\"/></services>\n";
    with_checkout_as(&log, "1.15.0", None, "jay-1.15.0", service, |ctx, st| {
        let mut calls = 0;
        let mut run = |dir: &Path, _: Emit<'_>| {
            calls += 1;
            zst_tarball(&dir.join("jay-1.15.0.tar.zst"), &["jay-1.15.0/x"]);
            Ok(())
        };
        fetch_sdist_stage_with(ctx, st, false, &mut run)
            .unwrap_or_else(|_| panic!("the stage fails"));
        assert_eq!(calls, 1, "the service runs once");
        assert!(
            ctx.ws.dir().join("source/x").exists(),
            "source/ is unpacked"
        );
        assert!(!unpacked_source_missing(ctx));
        assert_eq!(log.outcomes(StepKind::OscService), ["ok"]);
        assert_eq!(log.outcomes(StepKind::FetchSdist), ["ok"]);
        assert!(log.warnings.lock().unwrap().is_empty(), "no warning");
    });
}

#[test]
fn a_pass_that_makes_no_tarball_fails_its_steps_and_the_run_goes_on() {
    let log = Log::default();
    let service = "<services><service name=\"tar_scm\"/></services>\n";
    with_checkout_as(&log, "1.15.0", None, "jay-1.15.0", service, |ctx, st| {
        let mut run = |_: &Path, _: Emit<'_>| {
            Err(osc::Error::OscFailed {
                cmd: "service".into(),
                code: 1,
                stderr: "tar_scm: no such tag".into(),
            })
        };
        fetch_sdist_stage_with(ctx, st, false, &mut run)
            .unwrap_or_else(|_| panic!("the run goes on past a failed service"));
        assert_eq!(log.outcomes(StepKind::OscService), ["failed"]);
        assert_eq!(log.outcomes(StepKind::FetchSdist), ["failed"]);
        let warnings = log.warnings.lock().unwrap();
        assert!(
            warnings.iter().any(|w| w
                .starts_with("the _service ran but made no source tarball: osc service failed")
                && w.contains("tar_scm: no such tag")),
            "{warnings:?}"
        );
    });
}

#[test]
fn a_service_the_machine_cannot_run_stops_the_run() {
    let log = Log::default();
    let service = "<services><service name=\"tar_scm\"/></services>\n";
    with_checkout_as(&log, "1.15.0", None, "jay-1.15.0", service, |ctx, st| {
        let mut run = |_: &Path, _: Emit<'_>| {
            Err(osc::Error::Launch(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "osc: not found",
            )))
        };
        assert!(fetch_sdist_stage_with(ctx, st, false, &mut run).is_err());
    });
}

#[test]
fn the_pass_runs_only_when_the_checkout_needs_it() {
    let tar_scm = "<services><service name=\"tar_scm\"/></services>\n";
    let obs_scm = "<services><service name=\"obs_scm\" mode=\"buildtime\"/></services>\n";
    let never = |_: &Path, _: Emit<'_>| -> Result<(), osc::Error> {
        panic!("the service pass must not run here")
    };
    // A new package: GenerateSpec and its service made the tarball.
    with_checkout(
        Some(&["amqpstorm-1.0/x"]),
        "AMQPStorm-1.0",
        tar_scm,
        |ctx, st| {
            let mut run = never;
            fetch_sdist_stage_with(ctx, st, true, &mut run)
                .unwrap_or_else(|_| panic!("the stage fails"));
        },
    );
    // A plain tarball with no `_service`.
    with_checkout(
        Some(&["amqpstorm-1.0/x"]),
        "AMQPStorm-1.0",
        "",
        |ctx, st| {
            let mut run = never;
            fetch_sdist_stage_with(ctx, st, false, &mut run)
                .unwrap_or_else(|_| panic!("the stage fails"));
        },
    );
    // An OBS-made source.
    with_checkout(None, "AMQPStorm-1.0", obs_scm, |ctx, st| {
        let mut run = never;
        fetch_sdist_stage_with(ctx, st, false, &mut run)
            .unwrap_or_else(|_| panic!("the stage fails"));
    });
}

#[test]
fn a_replaced_release_loses_only_its_tarballs() {
    let dir = tempfile::tempdir().unwrap();
    for name in [
        "foo-1.0.tar.gz",
        "foo-1.0.tar",
        "foo-1.0.zip",
        "foo-1.0-py3-none-any.whl",
        "vendor.tar.zst",
        "registry.tar.zst",
        "foo-2.0.tar.gz",
        "foo.spec",
    ] {
        std::fs::write(dir.path().join(name), b"").unwrap();
    }
    let before = crate::pipeline::carry::tarballs_in(dir.path());
    let gone =
        crate::pipeline::archive::superseded_tarballs(&before, "1.0", "2.0", "foo-2.0.tar.gz", &[]);
    // The old tarball and its bare `.tar` go; the zip, the wheel and the Cargo
    // bundles stay, as they did before the list was unified.
    assert_eq!(gone, ["foo-1.0.tar", "foo-1.0.tar.gz"]);
}

#[test]
fn a_bare_tar_beside_the_source_is_not_unpacked() {
    // `deps.tar` sorts before `foo.tar.gz`, and no extractor reads a bare tar:
    // the source is the one that unpacks.
    with_checkout(
        Some(&["amqpstorm-1.0/x"]),
        "AMQPStorm-1.0",
        "",
        |ctx, st| {
            std::fs::write(ctx.checkout_dir.join("deps.tar"), b"not a source").unwrap();
            let found = crate::pipeline::archive::find_tarball(&ctx.checkout_dir, "1.0");
            assert_eq!(
                found.and_then(|p| p.file_name().map(|n| n.to_owned())),
                Some("AMQPStorm-1.0.tar.gz".into())
            );
            fetch_sdist_stage(ctx, st, false).unwrap_or_else(|_| panic!("the stage fails"));
            assert!(
                ctx.ws.dir().join("source/x").exists(),
                "the source unpacked"
            );
        },
    );
}

#[test]
fn the_missing_tarball_has_two_outcomes() {
    // The services ran and made no tarball, with or without an error.
    assert_eq!(
        classify_no_tarball(&SourcePass::Ran),
        NoTarball::ServicesRan(None)
    );
    assert_eq!(
        classify_no_tarball(&SourcePass::Failed("no such tag".into())),
        NoTarball::ServicesRan(Some("no such tag".into()))
    );
    // No service known to make one.
    assert_eq!(classify_no_tarball(&SourcePass::NotRun), NoTarball::Unmade);
    assert!(
        NoTarball::ServicesRan(Some("no such tag".into()))
            .warning()
            .contains("no such tag")
    );
    assert!(!NoTarball::Unmade.warning().contains("_service ran"));
}

#[test]
fn a_single_quoted_service_is_not_claimed_to_make_the_source() {
    // The pass only sees double-quoted `name="..."`: the warning must not say
    // that a `_service` makes the source when this one is not recognised.
    let service = "<services><service name='tar_scm'/></services>\n";
    let log = Log::default();
    with_checkout_as(&log, "1.0", None, "AMQPStorm-1.0", service, |ctx, st| {
        let mut run = |_: &Path, _: Emit<'_>| -> Result<(), osc::Error> {
            panic!("the pass does not see this service")
        };
        fetch_sdist_stage_with(ctx, st, false, &mut run)
            .unwrap_or_else(|_| panic!("the run goes on"));
    });
    assert_eq!(log.outcomes(StepKind::FetchSdist), ["failed"]);
    let warnings = log.warnings.lock().unwrap();
    assert!(
        warnings
            .iter()
            .any(|w| w.contains("no _service known to make one")),
        "{warnings:?}"
    );
}

#[test]
fn a_checkout_with_no_source_and_no_service_fails_with_a_warning() {
    let log = Log::default();
    with_checkout_as(&log, "1.0", None, "AMQPStorm-1.0", "", |ctx, st| {
        let mut run = |_: &Path, _: Emit<'_>| -> Result<(), osc::Error> {
            panic!("no service makes this source")
        };
        fetch_sdist_stage_with(ctx, st, false, &mut run)
            .unwrap_or_else(|_| panic!("the run goes on"));
    });
    assert_eq!(log.outcomes(StepKind::FetchSdist), ["failed"]);
    assert!(
        log.warnings
            .lock()
            .unwrap()
            .iter()
            .any(|w| w.contains("no _service known to make one"))
    );
}

#[test]
fn source_archives_finds_every_archive_spelling_but_no_bundle() {
    let dir = tempfile::tempdir().unwrap();
    let all = "foo-1.0.tar foo-1.0.tbz2 foo-1.0.txz foo-1.0.tzst foo-1.0.tar.gz foo-1.0.zip vendor.tar.zst vendor.tar.gz registry.tar.zst foo.spec";
    for name in all.split(' ') {
        std::fs::write(dir.path().join(name), b"").unwrap();
    }
    // A bare `.tar` is not a source: no extractor reads it.
    let mut found: Vec<String> = crate::pipeline::archive::source_archives(dir.path())
        .into_iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    found.sort();
    assert_eq!(
        found,
        [
            "foo-1.0.tar.gz",
            "foo-1.0.tbz2",
            "foo-1.0.txz",
            "foo-1.0.tzst",
            "foo-1.0.zip"
        ]
    );
}

#[test]
fn the_pass_will_run_for_a_service_without_its_tarball() {
    let tar_scm = "<services><service name=\"tar_scm\"/></services>\n";
    with_checkout(None, "AMQPStorm-1.0", tar_scm, |ctx, _| {
        assert!(pass_will_run(&ctx.checkout_dir, ctx.config));
    });
    with_checkout(
        Some(&["amqpstorm-1.0/x"]),
        "AMQPStorm-1.0",
        tar_scm,
        |ctx, _| {
            assert!(!pass_will_run(&ctx.checkout_dir, ctx.config));
        },
    );
}
