use super::*;
use crate::pipeline::sibling::*;

mod part2;

#[test]
fn the_ladder_runs_its_rungs_in_order_and_stops_at_the_first_resolution() {
    let mut seen: Vec<(usize, usize)> = Vec::new();
    let got: Result<&str, LadderEnd<()>> = run_ladder("first", 3, |round, history| {
        seen.push((round, history.len()));
        if round == 2 {
            Rung::Resolved("done")
        } else {
            Rung::Blocked(format!("reason {round}"))
        }
    });
    assert_eq!(got, Ok("done"));
    // Each round sees every earlier reason, the first turn's included.
    assert_eq!(seen, [(1, 1), (2, 2)]);
}

#[test]
fn restore_keeping_leaves_a_staged_file_that_overwrote_a_snapshotted_one() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    std::fs::write(dir.join("foo.spec"), "Name: foo\nVersion: 1.0\n").unwrap();
    std::fs::write(dir.join("fix.patch"), "--- orig\n+++ new\n").unwrap();
    std::fs::write(dir.join("other.patch"), "old other\n").unwrap();
    let snap = CheckoutSnapshot::take(dir);
    // The turn's staged files: fix.patch overwritten by stage_files, and
    // a spec that was staged too.
    std::fs::write(dir.join("fix.patch"), "staged content\n").unwrap();
    std::fs::write(dir.join("other.patch"), "agent edit\n").unwrap();
    std::fs::write(dir.join("foo.spec"), "staged spec\n").unwrap();
    std::fs::write(dir.join("new.patch"), "staged new\n").unwrap();
    let keep = vec![
        "fix.patch".to_string(),
        "foo.spec".to_string(),
        "new.patch".to_string(),
    ];
    struct Null;
    impl Reporter for Null {
        fn report(&self, _: &Event) {}
    }
    snap.restore_keeping(&Null, None, &keep);
    // The staged content of a kept file survives the rollback.
    assert_eq!(
        std::fs::read_to_string(dir.join("fix.patch")).unwrap(),
        "staged content\n"
    );
    assert!(dir.join("new.patch").exists());
    // A spec is never kept: a rejected spec is restored even when named.
    assert_eq!(
        std::fs::read_to_string(dir.join("foo.spec")).unwrap(),
        "Name: foo\nVersion: 1.0\n"
    );
    // A snapshotted file that is not kept is still rolled back.
    assert_eq!(
        std::fs::read_to_string(dir.join("other.patch")).unwrap(),
        "old other\n"
    );
}

#[test]
fn the_wheel_rung_waits_a_turn_after_an_npm_deferral() {
    // Round 2 is the wheel rung, as before, when nothing was deferred.
    assert_eq!(wheel_rung_round(None), 2);
    // A deferral in round 1 leaves round 2 to the agent turn; the wheel rung
    // runs in round 3.
    assert_eq!(wheel_rung_round(Some(1)), 3);
    // A deferral in round 2 leaves no round for the wheel rung.
    assert_eq!(wheel_rung_round(Some(2)), 4);
}

#[test]
fn a_ladder_stopped_on_the_budget_is_not_remembered() {
    // Only a ladder that ran its rungs is a memo; a patch run never is.
    assert!(should_remember_ladder(false, false));
    assert!(!should_remember_ladder(false, true));
    assert!(!should_remember_ladder(true, false));
    assert!(!should_remember_ladder(true, true));
    // A round skipped for the budget is a blocked reason the ladder ends on.
    let mut skipped = false;
    let got: Result<(), LadderEnd<()>> = run_ladder("t0", 3, |round, _| {
        if round >= 2 {
            skipped = true;
            return Rung::Blocked(format!("round {round} not run: budget exhausted"));
        }
        Rung::Blocked(format!("round {round} answered no_change"))
    });
    assert!(matches!(got, Err(LadderEnd::Exhausted(ref h)) if h.len() == 4));
    assert!(skipped);
}

#[test]
fn a_spent_ladder_reports_every_reason_and_an_abort_ends_it_at_once() {
    let spent: Result<(), LadderEnd<()>> =
        run_ladder("t0", LADDER_ROUNDS, |r, _| Rung::Blocked(format!("r{r}")));
    assert_eq!(
        spent,
        Err(LadderEnd::Exhausted(vec![
            "t0".into(),
            "r1".into(),
            "r2".into(),
            "r3".into()
        ]))
    );
    let mut calls = 0;
    let aborted: Result<(), LadderEnd<&str>> = run_ladder("t0", 3, |_, _| {
        calls += 1;
        Rung::Abort("patch it")
    });
    assert_eq!(aborted, Err(LadderEnd::Abort("patch it")));
    assert_eq!(calls, 1);
}

#[test]
fn a_resolver_round_says_blocked_is_refused_and_lists_the_options() {
    let text = ladder_context(
        2,
        3,
        &["Source10 missing".into(), "still missing".into()],
        Some("base"),
    );
    for needle in [
        "ROUND 2 of 3",
        "NOT an accepted",
        "Source10 missing",
        "stage_files",
        "patch_dependencies",
        "base",
    ] {
        assert!(text.contains(needle), "{needle}: {text}");
    }
}

#[test]
fn sibling_names_come_from_the_spec_dependencies() {
    let spec = "Name: x\nBuildRequires:  %{python_module numpy >= 1}\nRequires: nvidia-cutlass-dsl-libs-base >= 4\nRequires: python3dist(torch)\n";
    let available: Vec<String> = [
        "python-numpy",
        "nvidia-cutlass-dsl-libs-base",
        "python-torch",
        "python-unrelated",
    ]
    .map(String::from)
    .into();
    let got = sibling_names(spec, &available, "python-x");
    assert!(got.contains(&"python-numpy".to_string()), "{got:?}");
    assert!(
        got.contains(&"nvidia-cutlass-dsl-libs-base".to_string()),
        "{got:?}"
    );
    assert!(got.contains(&"python-torch".to_string()), "{got:?}");
    assert!(!got.contains(&"python-unrelated".to_string()), "{got:?}");
    // A name in prose is not a dependency: the reason text no longer names one.
    assert!(sibling_names("Name: x\n", &available, "python-x").is_empty());
}

#[test]
fn a_stage_files_request_is_executed_with_this_packages_trees() {
    let root = tempfile::tempdir().unwrap();
    let work = root.path().to_path_buf();
    let state_root = work.join("state");
    let ws = Workspace::open(&state_root, "python-open-webui").unwrap();
    std::fs::create_dir_all(ws.sdist_dir().join("top")).unwrap();
    std::fs::write(
        ws.sdist_dir().join("top/package-lock.json"),
        b"{\"lockfileVersion\":3}",
    )
    .unwrap();
    std::os::unix::fs::symlink(ws.sdist_dir(), ws.dir().join("source")).unwrap();
    let checkout = work.join("proj/python-open-webui");
    std::fs::create_dir_all(&checkout).unwrap();
    let mut config = super::hygiene_tests::config("open-webui", &state_root);
    config.work_dir = work.clone();
    config.project = "proj".into();
    let request = |from: &str, to: &str| crate::stage::StageFile {
        from: from.into(),
        to: to.into(),
        package: None,
    };
    let (done, refused) = stage_for_config(
        &config,
        &checkout,
        &[
            request("source/top/package-lock.json", "package-lock.json"),
            request("../x", "bad"),
        ],
    );
    assert_eq!(done, ["package-lock.json"]);
    assert_eq!(refused.len(), 1, "{refused:?}");
    assert_eq!(
        std::fs::read(checkout.join("package-lock.json")).unwrap(),
        b"{\"lockfileVersion\":3}"
    );
    assert!(!checkout.join("bad").exists());
}

#[test]
fn the_npm_route_copies_the_lockfile_and_writes_the_service_itself() {
    let dir = tempfile::tempdir().unwrap();
    let file = std::fs::File::create(dir.path().join("pkg-1.0.tar.gz")).unwrap();
    let enc = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
    let mut ar = tar::Builder::new(enc);
    for (name, body) in [
        ("pkg-1.0/frontend/deep/package-lock.json", &b"deep"[..]),
        ("pkg-1.0/package-lock.json", &b"{\"lockfileVersion\":3}"[..]),
        ("pkg-1.0/node_modules/x/package-lock.json", &b"nope"[..]),
    ] {
        let mut h = tar::Header::new_gnu();
        h.set_size(body.len() as u64);
        h.set_mode(0o644);
        h.set_cksum();
        ar.append_data(&mut h, name, body).unwrap();
    }
    ar.into_inner().unwrap().finish().unwrap();
    // No `_service` yet: one is written.
    let done = stage_npm_vendor_route(dir.path(), "1.0").unwrap();
    assert_eq!(done.len(), 2, "{done:?}");
    assert_eq!(
        std::fs::read(dir.path().join("package-lock.json")).unwrap(),
        b"{\"lockfileVersion\":3}"
    );
    let service = std::fs::read_to_string(dir.path().join("_service")).unwrap();
    assert!(
        service.contains("name=\"node_modules\"") && service.contains("package-lock.json"),
        "{service}"
    );
    osc::validate_service_xml(service.as_bytes()).unwrap();
    // Again: nothing is duplicated.
    stage_npm_vendor_route(dir.path(), "1.0").unwrap();
    assert_eq!(
        std::fs::read_to_string(dir.path().join("_service")).unwrap(),
        service
    );
    // An existing `_service` keeps its services.
    std::fs::write(
        dir.path().join("_service"),
        "<services>\n  <service name=\"download_files\" mode=\"manual\"/>\n</services>\n",
    )
    .unwrap();
    stage_npm_vendor_route(dir.path(), "1.0").unwrap();
    let service = std::fs::read_to_string(dir.path().join("_service")).unwrap();
    assert!(
        service.contains("download_files") && service.contains("node_modules"),
        "{service}"
    );
    osc::validate_service_xml(service.as_bytes()).unwrap();
}

// ── the ladder end to end, with a scripted turn runner ──────────────────

fn scripted_outcome(action: crate::agent::Action, reasoning: &str) -> crate::agent::TurnOutcome {
    let mut out = pipeline_wheel_outcome("x");
    out.action = action;
    out.reasoning = reasoning.into();
    out
}

fn blocked(why: &str) -> crate::agent::TurnOutcome {
    scripted_outcome(crate::agent::Action::Blocked { why: why.into() }, why)
}

fn stages(mut out: crate::agent::TurnOutcome, from: &str, to: &str) -> crate::agent::TurnOutcome {
    out.stage_files.push(crate::stage::StageFile {
        from: from.into(),
        to: to.into(),
        package: None,
    });
    out
}

struct Case {
    _root: tempfile::TempDir,
    result: Result<TurnEffect, PipelineResult>,
    /// The context each scripted turn was given, in order.
    contexts: Vec<String>,
    notes: String,
    checkout: PathBuf,
    state_root: PathBuf,
    config_key: u64,
}

/// Run `resolve_blocked_with` on a package whose first turn answered
/// `first`, with `turns` as the answers of the resolver rounds. `siblings`
/// are other packages of the project.
fn run_case(
    rpm_name: &str,
    force: bool,
    siblings: &[&str],
    first: crate::agent::TurnOutcome,
    turns: Vec<crate::agent::TurnOutcome>,
    wheel_lookups: &std::cell::Cell<usize>,
    check: Option<(&str, &str)>,
) -> Case {
    let root = tempfile::tempdir().unwrap();
    let work = root.path().join("work");
    let state_root = work.join("state");
    let mut config = super::hygiene_tests::config(rpm_name, &state_root);
    config.rpm_name = rpm_name.into();
    config.work_dir = work.clone();
    config.project = "proj".into();
    config.force = force;
    config.no_agent = false;
    config.budget_usd = 100.0;
    let ws = Workspace::open(&state_root, rpm_name).unwrap();
    std::fs::create_dir_all(ws.dir().join("source")).unwrap();
    std::fs::write(ws.dir().join("source/package-lock.json"), b"{\"lock\":3}").unwrap();
    let checkout = work.join("proj").join(rpm_name);
    std::fs::create_dir_all(&checkout).unwrap();
    for s in siblings {
        std::fs::create_dir_all(work.join("proj").join(s)).unwrap();
    }
    let spec_path = checkout.join(format!("{rpm_name}.spec"));
    // A spec rpmspec accepts: the acceptance gate now checks the edit.
    let before = format!(
        "Name: {rpm_name}\nVersion: 1.0\nRelease: 0\nSummary: test\nLicense: MIT\n\n%check\n%pytest\n"
    );
    std::fs::write(&spec_path, &before).unwrap();
    let run = ap_core::run::RunDir::allocate(&work).unwrap();
    let pkg_run = run.pkg(rpm_name);
    let ctx = RunCtx {
        config: &config,
        pkg_run: &pkg_run,
        reporter: &ap_core::sink::NULL_REPORTER,
        ws,
        rpm_name: rpm_name.into(),
        obs_name: rpm_name.into(),
        checkout_dir: checkout.clone(),
        spec_path: spec_path.clone(),
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
        last_failure_shape: None,
        relaxed_from_spec: None,
        post_review_done: false,
        post_review_revert: None,
    };
    let snapshot = CheckoutSnapshot::take(&checkout);
    let step = StepDir {
        path: root.path().join("step"),
        id: ap_core::event::StepId(1),
    };
    std::fs::create_dir_all(&step.path).unwrap();
    // A failed %check build: its log, the extraction naming the section,
    // and a `.changes` file that already says what the rung would add (so no
    // `osc vc` runs).
    let log_path = root.path().join("build.log");
    let mut hint = crate::log::ExtractedLog::default();
    if let Some((log, message)) = check {
        std::fs::write(&log_path, log).unwrap();
        hint.failed_section = Some("%check".into());
        std::fs::write(
            checkout.join(format!("{rpm_name}.changes")),
            format!(
                "{}\nMon Jan 1 00:00:00 UTC 2026 - x@y\n\n- {message}\n\n{}\n",
                "-".repeat(67),
                "-".repeat(67)
            ),
        )
        .unwrap();
    }
    let site = LadderSite {
        build_log: check.map(|_| log_path.as_path()),
        task: crate::agent::Task::Review,
        event_task: ap_core::event::Task::Review,
        attempt: 0,
        hint: check.map(|_| &hint),
        context: None,
        apply_label: "Review",
        before: &before,
        snapshot: &snapshot,
        step: &step,
        staged: &[],
    };
    let queue = std::cell::RefCell::new(std::collections::VecDeque::from(turns));
    let contexts = std::cell::RefCell::new(Vec::new());
    let lookup = |_: &PipelineConfig| {
        wheel_lookups.set(wheel_lookups.get() + 1);
        Err("no PyPI in this test".to_string())
    };
    let result = resolve_blocked_with(
        &ctx,
        &mut st,
        site,
        &first,
        "first turn blocked".into(),
        &lookup,
        &mut |_, _, site, _, _, context| {
            contexts.borrow_mut().push(context.to_string());
            let out = queue
                .borrow_mut()
                .pop_front()
                .expect("the ladder asked for more turns than the script has");
            // A speculative file the turn left in the checkout.
            std::fs::write(site.snapshot.checkout_dir.join("junk.patch"), b"x").unwrap();
            Ok((
                StepDir {
                    path: site.step.path.clone(),
                    id: ap_core::event::StepId(2),
                },
                out,
            ))
        },
    );
    let notes = std::fs::read_to_string(ctx.ws.notes()).unwrap_or_default();
    let config_key = ladder_key(&config, &before);
    let contexts = contexts.into_inner();
    Case {
        _root: root,
        result,
        contexts,
        notes,
        checkout,
        state_root,
        config_key,
    }
}

#[test]
fn a_staged_file_is_kept_through_the_ladder_and_blocked_is_not_a_resolution() {
    let lookups = std::cell::Cell::new(0);
    let case = run_case(
        "open-webui-thing",
        false,
        &[],
        blocked("Source10 is missing"),
        vec![
            // Round 1 changes nothing.
            scripted_outcome(crate::agent::Action::NoChange, "nothing to do"),
            // Round 2 asks the pipeline for the file, answering blocked: the
            // file is staged and kept, but the round did not resolve anything.
            stages(
                blocked("I cannot copy it"),
                "source/package-lock.json",
                "package-lock.json",
            ),
            // Round 3 changes nothing either.
            scripted_outcome(crate::agent::Action::NoChange, "still nothing"),
        ],
        &lookups,
        None,
    );
    // Every rung failed: the ladder is spent, not resolved.
    assert!(
        matches!(
            case.result,
            Err(PipelineResult::Failed {
                kind: FailureKind::Exhausted,
                ..
            })
        ),
        "{:?}",
        case.result
    );
    assert_eq!(case.contexts.len(), 3);
    // Round 2 was told what round 1 came to, and round 3 what round 2 came to.
    assert!(
        case.contexts[1].contains("ROUND 2 of 3")
            && case.contexts[1].contains("answered no_change"),
        "{}",
        case.contexts[1]
    );
    assert!(
        case.contexts[2].contains("ROUND 3 of 3"),
        "{}",
        case.contexts[2]
    );
    // The staged file stays through the later rollbacks: round 3 had it.
    assert_eq!(
        std::fs::read(case.checkout.join("package-lock.json")).unwrap(),
        b"{\"lock\":3}"
    );
    assert!(case.notes.contains("staged files"), "{}", case.notes);
    // A spent ladder of a package's own spec leaves its memo.
    let state = PkgState::load(&Workspace::open(&case.state_root, "open-webui-thing").unwrap());
    assert_eq!(state.ladder_spec_hash, Some(case.config_key));
}

/// A reason that names a sibling is only text: the ladder does not turn it
/// into a patch request. A sibling is asked for through `patch_dependencies`.
#[test]
fn a_reason_naming_a_sibling_does_not_request_its_patch() {
    let lookups = std::cell::Cell::new(0);
    let case = run_case(
        "python-tokenspeed-mla",
        false,
        &["python-nvidia-cutlass-dsl"],
        blocked("The fix belongs in the sibling python-nvidia-cutlass-dsl, not here"),
        vec![
            blocked("reason one"),
            blocked("reason two"),
            blocked("reason three"),
        ],
        &lookups,
        None,
    );
    assert!(
        !matches!(case.result, Err(PipelineResult::MissingDeps { .. })),
        "{:?}",
        case.result
    );
}
