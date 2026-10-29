use super::*;
use ratatui::backend::TestBackend;
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

const DIFF: &str = "--- foo.spec\n+++ foo.spec\n@@ -1,3 +1,3 @@ Name: foo\n Name: foo\n-Version: 1.0\n+Version: 2.0\n License: MIT\n@@ -20,2 +20,2 @@\n-a\n+b\n c\n";

struct Fake {
    diff: Result<String, String>,
    preview: Result<SubmitPreview, SubmitFailure>,
    submit: Result<String, SubmitFailure>,
    submitted: Mutex<Vec<String>>,
}

impl Fake {
    fn ok() -> Arc<dyn PackageBackend> {
        Arc::new(Self {
            diff: Ok(DIFF.to_string()),
            preview: Ok(SubmitPreview {
                summary: "home:me/foo -> Base/foo (1.0 -> 2.0)".into(),
                source: "home:me/foo".into(),
                target: "Base/foo".into(),
                description: "Update foo to 2.0.\n\nChangelog:\n- Update to 2.0".into(),
            }),
            submit: Ok("123".into()),
            submitted: Mutex::new(Vec::new()),
        })
    }
}

impl PackageBackend for Fake {
    fn diff(&self, _: &str) -> Result<String, String> {
        self.diff.clone()
    }
    fn preview(&self, _: &str) -> Result<SubmitPreview, SubmitFailure> {
        self.preview.clone()
    }
    fn submit(&self, row: &str) -> Result<String, SubmitFailure> {
        self.submitted.lock().unwrap().push(row.to_string());
        self.submit.clone()
    }
}

fn state(status: PkgStatus, finished: bool) -> TuiState {
    let mut s = TuiState::new(RunScope::default());
    let p = s.pkg_mut("foo");
    p.status = status;
    p.finished = finished;
    s
}

fn screen(state: &TuiState, browse: &BrowseState, w: u16, h: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
    let mut table_state = TableState::default();
    table_state.select(Some(0));
    terminal
        .draw(|f| {
            render(f, state, &mut table_state, None, browse, None);
        })
        .unwrap();
    let buf = terminal.backend().buffer();
    (0..buf.area.height)
        .map(|y| {
            (0..buf.area.width)
                .map(|x| buf[(x, y)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn next_job(rx: &Receiver<Job>) -> Job {
    rx.recv_timeout(Duration::from_secs(5)).unwrap()
}

#[test]
fn the_diff_loads_in_the_background_and_renders() {
    let st = state(PkgState::new(String::new()).status, false);
    let (tx, rx) = channel();
    let mut b = BrowseState::default();
    let backend = Fake::ok();
    open_diff(&st, &mut b, "foo", Some(&backend), &tx);
    assert!(screen(&st, &b, 100, 30).contains("reading the diff"));
    apply_job(
        &mut TuiState::new(RunScope::default()),
        &mut b,
        next_job(&rx),
    );
    let out = screen(&st, &b, 100, 30);
    assert!(out.contains("1 file changed, +2 \u{2212}2"), "{out}");
    assert!(
        out.contains("Version: 2.0") && out.contains("foo.spec"),
        "{out}"
    );
    assert!(out.contains("j/k"), "keys hint: {out}");
}

#[test]
fn a_failed_diff_shows_the_error_and_can_be_retried_or_replaced() {
    let mut st = state(PkgStatus::Running, false);
    st.pkg_mut("foo")
        .spec_diffs
        .push("- Version: 1\n+ Version: 2\n".into());
    let backend: Arc<dyn PackageBackend> = Arc::new(Fake {
        diff: Err("osc rdiff failed: HTTP Error 502".into()),
        preview: Err(SubmitFailure::Other("x".into())),
        submit: Err(SubmitFailure::Other("x".into())),
        submitted: Mutex::new(Vec::new()),
    });
    let (tx, rx) = channel();
    let mut b = BrowseState::default();
    open_diff(&st, &mut b, "foo", Some(&backend), &tx);
    apply_job(&mut st, &mut b, next_job(&rx));
    let out = screen(&st, &b, 100, 30);
    assert!(
        out.contains("HTTP Error 502") && out.contains("r retry"),
        "{out}"
    );
    assert!(out.contains("recorded by this run"), "{out}");
    let Some(Modal::Diff(v)) = &mut b.modal else {
        panic!()
    };
    assert_eq!(diff_key(v, KeyCode::Char('r')), DiffAction::Retry);
    assert_eq!(diff_key(v, KeyCode::Char('s')), DiffAction::Recorded);
    redo_diff(&st, &mut b, DiffAction::Recorded, Some(&backend), &tx);
    assert!(screen(&st, &b, 100, 30).contains("Version: 2"));
    // The older, failed request's answer is ignored once the view moved on.
    apply_job(
        &mut st,
        &mut b,
        Job::Diff {
            row: "foo".into(),
            seq: 1,
            result: Err("late".into()),
        },
    );
    assert!(!screen(&st, &b, 100, 30).contains("late"));
}

#[test]
fn without_a_backend_the_recorded_spec_changes_are_shown_or_none_said() {
    let mut st = state(PkgStatus::Running, false);
    let (tx, _rx) = channel();
    let mut b = BrowseState::default();
    open_diff(&st, &mut b, "foo", None, &tx);
    assert!(screen(&st, &b, 100, 30).contains("No changes recorded yet"));
    st.pkg_mut("foo").spec_diffs.push("- a\n+ b\n".into());
    open_diff(&st, &mut b, "foo", None, &tx);
    let out = screen(&st, &b, 100, 30);
    assert!(
        out.contains("recorded by this run") && out.contains("1 file changed"),
        "{out}"
    );
}

fn fake_explanation() -> Explanation {
    let mut ex = Explanation {
        package: "foo".into(),
        ..Default::default()
    };
    ex.outcome.result = "failed".into();
    ex.outcome.detail = Some("gave up after 2 attempts".into());
    ex.unknown = (0..60).map(|i| format!("unknown thing {i}")).collect();
    ex
}

#[test]
fn the_explanation_loads_in_the_background_scrolls_and_closes() {
    let mut st = state(PkgStatus::Running, false);
    let (tx, rx) = channel();
    let mut b = BrowseState::default();
    let loader: ExplainLoader = Arc::new(|row| {
        assert_eq!(row, "foo");
        Ok(fake_explanation())
    });
    open_explain(&mut b, "foo", Some(&loader), &tx);
    assert!(screen(&st, &b, 100, 30).contains("reading the run files"));
    apply_job(&mut st, &mut b, next_job(&rx));
    let out = screen(&st, &b, 100, 30);
    assert!(
        out.contains("Explain: foo") && out.contains("gave up after 2 attempts"),
        "{out}"
    );
    assert!(out.contains("PgUp/PgDn"), "keys hint: {out}");
    let Some(Modal::Explain(v)) = &b.modal else {
        panic!()
    };
    assert_eq!(explain_key(v, KeyCode::Char('G')), ExplainAction::None);
    assert!(v.scroll.get() > 0);
    let out = screen(&st, &b, 100, 30);
    assert!(
        out.contains("unknown thing 59") && !out.contains("Package foo"),
        "{out}"
    );
    assert_eq!(explain_key(v, KeyCode::Home), ExplainAction::None);
    assert_eq!(v.scroll.get(), 0);
    assert_eq!(explain_key(v, KeyCode::Char('q')), ExplainAction::Close);
    assert_eq!(explain_key(v, KeyCode::Esc), ExplainAction::Close);
}

#[test]
fn a_failed_explanation_shows_the_error_and_a_late_answer_is_ignored() {
    let mut st = state(PkgStatus::Running, false);
    let (tx, rx) = channel();
    let mut b = BrowseState::default();
    let loader: ExplainLoader = Arc::new(|_| Err("nothing about foo in /w".into()));
    open_explain(&mut b, "foo", Some(&loader), &tx);
    apply_job(&mut st, &mut b, next_job(&rx));
    assert!(screen(&st, &b, 100, 30).contains("nothing about foo in /w"));
    apply_job(
        &mut st,
        &mut b,
        Job::Explain {
            row: "foo".into(),
            seq: 999,
            result: Ok(fake_explanation()),
        },
    );
    assert!(!screen(&st, &b, 100, 30).contains("gave up"));
    // Without a loader the modal says why instead of waiting forever.
    open_explain(&mut b, "foo", None, &tx);
    assert!(screen(&st, &b, 100, 30).contains("work directory of this run is not known"));
}

#[test]
fn an_empty_diff_says_there_are_no_changes() {
    let st = state(PkgStatus::Running, false);
    let mut b = BrowseState::default();
    b.modal = Some(Modal::Diff(DiffView::new(
        "foo",
        DiffSource::Origin,
        DiffLoad::Ready(diffview::parse("")),
        false,
        1,
    )));
    assert!(screen(&st, &b, 100, 30).contains("No changes: the package is identical"));
}

#[test]
fn diff_keys_scroll_jump_search_and_close() {
    let mut text = String::from("--- f\n+++ f\n");
    for h in 0..3 {
        text.push_str(&format!("@@ -{0},40 +{0},40 @@\n", h * 100 + 1));
        for i in 0..40 {
            text.push_str(&format!(" line {h}-{i}\n"));
        }
    }
    let st = state(PkgStatus::Running, false);
    let mut b = BrowseState::default();
    b.modal = Some(Modal::Diff(DiffView::new(
        "foo",
        DiffSource::Origin,
        DiffLoad::Ready(diffview::parse(&text)),
        false,
        1,
    )));
    screen(&st, &b, 100, 30);
    let Some(Modal::Diff(v)) = &mut b.modal else {
        panic!()
    };
    diff_key(v, KeyCode::Char('j'));
    assert_eq!(v.scroll.get(), 1);
    diff_key(v, KeyCode::Char('k'));
    diff_key(v, KeyCode::Char('k'));
    assert_eq!(v.scroll.get(), 0);
    diff_key(v, KeyCode::Char(']'));
    assert_eq!(v.scroll.get(), 2);
    diff_key(v, KeyCode::Char('n'));
    assert_eq!(v.scroll.get(), 43);
    diff_key(v, KeyCode::Char('['));
    assert_eq!(v.scroll.get(), 2);
    diff_key(v, KeyCode::Char('G'));
    assert_eq!(v.scroll.get(), v.max_scroll());
    diff_key(v, KeyCode::Char('g'));
    diff_key(v, KeyCode::PageDown);
    assert_eq!(v.scroll.get(), v.viewport.get() - 1);
    diff_key(v, KeyCode::Char('g'));
    for c in ['/', '2', '-', '7', '\n'] {
        let code = if c == '\n' {
            KeyCode::Enter
        } else {
            KeyCode::Char(c)
        };
        diff_key(v, code);
    }
    let first = v.scroll.get();
    assert!(first > 0);
    assert!(
        v.with_rendered(100, |r| r.plain[first].contains("2-7"))
            .unwrap()
    );
    assert_eq!(diff_key(v, KeyCode::Esc), DiffAction::None);
    assert!(v.search.is_none());
    assert_eq!(diff_key(v, KeyCode::Esc), DiffAction::Close);
    assert_eq!(diff_key(v, KeyCode::Char('q')), DiffAction::Close);
}

fn built() -> TuiState {
    state(PkgStatus::Succeeded, true)
}

#[test]
fn a_package_that_did_not_build_is_never_offered() {
    for status in [
        PkgStatus::Failed,
        PkgStatus::Running,
        PkgStatus::Queued,
        PkgStatus::MissingDeps,
    ] {
        let mut st = state(status, true);
        let (tx, _rx) = channel();
        let mut b = BrowseState::default();
        assert!(submit_rows(&st, None, true).is_empty());
        open_submit(&mut st, &mut b, vec!["foo".into()], Some(&Fake::ok()), &tx);
        assert!(b.modal.is_none());
        assert!(
            st.last_global_log
                .unwrap()
                .message
                .contains("not submitted")
        );
    }
}

#[test]
fn submitting_shows_what_is_sent_then_makes_one_request_on_yes() {
    let mut st = built();
    let (tx, rx) = channel();
    let mut b = BrowseState::default();
    let fake = Fake::ok();
    let rows = submit_rows(&st, Some("foo"), false);
    open_submit(&mut st, &mut b, rows, Some(&fake), &tx);
    assert!(screen(&st, &b, 110, 36).contains("checking foo"));
    apply_job(&mut st, &mut b, next_job(&rx));
    let out = screen(&st, &b, 110, 36);
    for want in [
        "home:me/foo (branch project)",
        "Base/foo (origin project)",
        "1 file changed, +2",
        "Update to 2.0",
        "y create 1 submit request",
    ] {
        assert!(out.contains(want), "missing {want:?}:\n{out}");
    }
    let Some(Modal::Submit(m)) = &mut b.modal else {
        panic!()
    };
    assert_eq!(submit_key(m, KeyCode::Char('x')), SubmitAction::None);
    assert_eq!(submit_key(m, KeyCode::Char('y')), SubmitAction::Confirm);
    confirm_submit(&mut st, &mut b, Some(&fake), &tx);
    assert!(b.modal.is_none());
    assert_eq!(st.packages["foo"].sr, Some(SrMark::Submitting));
    apply_job(&mut st, &mut b, next_job(&rx));
    assert_eq!(st.packages["foo"].sr, Some(SrMark::Created("123".into())));
    assert!(screen(&st, &b, 110, 36).contains("SR #123"));
    assert!(
        st.last_global_log
            .as_ref()
            .unwrap()
            .message
            .contains("#123")
    );

    // A second time asks again, with a warning, and `A` leaves it out.
    assert!(submit_rows(&st, None, true).is_empty());
    open_submit(&mut st, &mut b, vec!["foo".into()], Some(&fake), &tx);
    apply_job(&mut st, &mut b, next_job(&rx));
    let out = screen(&st, &b, 110, 36);
    assert!(
        out.contains("already created") && out.contains("second request"),
        "{out}"
    );
}

#[test]
fn no_is_the_default_and_nothing_is_sent() {
    let mut st = built();
    let (tx, rx) = channel();
    let mut b = BrowseState::default();
    let fake = Fake::ok();
    open_submit(&mut st, &mut b, vec!["foo".into()], Some(&fake), &tx);
    apply_job(&mut st, &mut b, next_job(&rx));
    let Some(Modal::Submit(m)) = &mut b.modal else {
        panic!()
    };
    assert_eq!(submit_key(m, KeyCode::Char('n')), SubmitAction::Close);
    assert_eq!(submit_key(m, KeyCode::Esc), SubmitAction::Close);
    assert_eq!(submit_key(m, KeyCode::Enter), SubmitAction::None);
    assert_eq!(st.packages["foo"].sr, None);
}

#[test]
fn an_ineligible_or_already_open_package_cannot_be_confirmed() {
    let mut st = built();
    let (tx, rx) = channel();
    let mut b = BrowseState::default();
    let fake: Arc<dyn PackageBackend> = Arc::new(Fake {
        diff: Err("no network".into()),
        preview: Err(SubmitFailure::Existing("77".into())),
        submit: Err(SubmitFailure::Other("x".into())),
        submitted: Mutex::new(Vec::new()),
    });
    open_submit(&mut st, &mut b, vec!["foo".into()], Some(&fake), &tx);
    apply_job(&mut st, &mut b, next_job(&rx));
    assert_eq!(st.packages["foo"].sr, Some(SrMark::Existing("77".into())));
    let out = screen(&st, &b, 110, 30);
    assert!(
        out.contains("request #77 is already open") && out.contains("any key closes"),
        "{out}"
    );
    let Some(Modal::Submit(m)) = &mut b.modal else {
        panic!()
    };
    assert_eq!(submit_key(m, KeyCode::Char('y')), SubmitAction::Close);
    b.modal = None;
    assert!(screen(&st, &b, 140, 30).contains("SR #77 (open)"));
}

#[test]
fn a_failed_request_is_marked_and_logged() {
    let mut st = built();
    let (tx, rx) = channel();
    let mut b = BrowseState::default();
    let fake: Arc<dyn PackageBackend> = Arc::new(Fake {
        diff: Ok(DIFF.into()),
        preview: Fake::ok_preview(),
        submit: Err(SubmitFailure::Other("HTTP 403".into())),
        submitted: Mutex::new(Vec::new()),
    });
    open_submit(&mut st, &mut b, vec!["foo".into()], Some(&fake), &tx);
    apply_job(&mut st, &mut b, next_job(&rx));
    confirm_submit(&mut st, &mut b, Some(&fake), &tx);
    apply_job(&mut st, &mut b, next_job(&rx));
    assert_eq!(
        st.packages["foo"].sr,
        Some(SrMark::Failed("HTTP 403".into()))
    );
    assert!(screen(&st, &b, 140, 30).contains("submit failed: HTTP 403"));
    // A failed one is offered again by `A`.
    assert_eq!(submit_rows(&st, None, true), vec!["foo".to_string()]);
}

#[test]
fn without_a_backend_the_submit_keys_say_so() {
    let mut st = built();
    let (tx, _rx) = channel();
    let mut b = BrowseState::default();
    open_submit(&mut st, &mut b, vec!["foo".into()], None, &tx);
    assert!(b.modal.is_none());
    assert!(
        st.last_global_log
            .unwrap()
            .message
            .contains("update-project")
    );
}

#[test]
fn a_request_in_flight_is_not_started_twice() {
    let mut st = built();
    st.pkg_mut("foo").sr = Some(SrMark::Submitting);
    let (tx, _rx) = channel();
    let mut b = BrowseState::default();
    assert!(submit_rows(&st, None, true).is_empty());
    open_submit(&mut st, &mut b, vec!["foo".into()], Some(&Fake::ok()), &tx);
    assert!(b.modal.is_none());
}

impl Fake {
    fn ok_preview() -> Result<SubmitPreview, SubmitFailure> {
        Ok(SubmitPreview {
            summary: "s".into(),
            source: "a/foo".into(),
            target: "b/foo".into(),
            description: "d".into(),
        })
    }
}
