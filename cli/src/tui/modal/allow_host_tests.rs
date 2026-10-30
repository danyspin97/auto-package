//! The allow-host prompt and its keys, driven as plain state: no terminal, and
//! the user's file only in a temporary directory.

use super::*;
use ap_build::allowed_hosts::{AllowedHosts, Asker, Origin, Use, Via, refusal_hint};
use ap_core::event::{Event, HostScope, HostSource};
use ap_core::sink::Reporter;
use ratatui::backend::TestBackend;
use std::sync::Mutex;

/// Refuse `hosts` for `use_` the way the tool does (the refusal record), and
/// return a failure text that names them as the tool's messages do.
fn refuse(list: &AllowedHosts, package: &str, hosts: &[&str], use_: Use) -> String {
    let asker = match use_ {
        Use::Service => Asker::service(package, "cargo_vendor"),
        Use::Source => Asker::source(package),
    };
    hosts
        .iter()
        .map(|h| {
            let _ = list.check_host(h, use_, &asker);
            format!("403 {h}:443: {}", refusal_hint(h, use_))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// A state whose package `name` failed with `reason`.
fn failed(name: &str, reason: String) -> TuiState {
    let mut state = TuiState::new(RunScope::default());
    let p = state.pkg_mut(name);
    p.status = PkgStatus::Failed;
    p.finished = true;
    p.fail_reason = Some(reason);
    state
}

fn names(cands: &[Candidate]) -> Vec<&str> {
    cands.iter().map(|c| c.host.as_str()).collect()
}

/// A prompt for `package` with one service refusal per host, already drawn on
/// a 200x30 terminal (the space the tests were written for).
fn modal_for(package: &str, hosts: &[&str], path: Option<PathBuf>) -> AllowHostModal {
    let m = AllowHostModal::new(
        package.to_string(),
        hosts
            .iter()
            .map(|h| Candidate {
                host: h.to_string(),
                askers: vec![Asker::service(package, "cargo_vendor")],
            })
            .collect(),
        path,
    );
    drawn(&m, 200, 30);
    m
}

/// Draw `m` the way the run loop does, on a `w` x `h` terminal, so the prompt
/// knows the body size its keys check.
fn drawn(m: &AllowHostModal, w: u16, h: u16) {
    let mut term = ratatui::Terminal::new(TestBackend::new(w, h)).unwrap();
    let area = crate::tui::modal::render::centered(Rect::new(0, 0, w, h), 90, 90);
    term.draw(|f| render_allow_host(f, m, area)).unwrap();
}

/// Type `text` into the prompt, one key per character.
fn type_text(m: &mut AllowHostModal, text: &str) {
    for c in text.chars() {
        allow_host_key(m, KeyCode::Char(c));
    }
}

#[test]
fn a_refused_host_is_pending_until_it_is_allowed() {
    let list = AllowedHosts::empty();
    let reason = refuse(
        &list,
        "foo",
        &["a.example.org", "b.example.org"],
        Use::Service,
    );
    let mut state = failed("foo", reason);
    let (name, cands) = allow_target(&state, Some("foo"), &list).unwrap();
    assert_eq!(name, "foo");
    assert_eq!(names(&cands), ["a.example.org", "b.example.org"]);
    assert_eq!(allow_target(&state, Some("bar"), &list), None);
    assert_eq!(allow_target(&state, None, &list), None);
    // While pending, a retry is not offered: it would be refused again.
    let p = &state.packages["foo"];
    assert!(!retry_after_allow("foo", p, &list));
    assert_eq!(retry_label("foo", p, &list), None);

    let added = allow_for_run(
        &mut state,
        "foo",
        &["a.example.org".into(), "b.example.org".into()],
        &list,
    );
    assert_eq!(added, ["a.example.org", "b.example.org"]);
    assert_eq!(allow_target(&state, Some("foo"), &list), None);
    assert_eq!(
        retry_label("foo", &state.packages["foo"], &list),
        Some("host allowed")
    );
    assert!(list.allows("a.example.org", Use::Service));
    assert_eq!(
        list.origin_of("a.example.org", Use::Service),
        Some(Origin::Run(Via::Tui))
    );
    let log: Vec<String> = state.run_log.iter().map(|l| l.message.clone()).collect();
    assert!(
        log.iter().any(|l| l.contains("press r to retry foo")),
        "{log:?}"
    );
}

#[test]
fn a_forged_refusal_in_service_output_does_not_make_a_host_pending() {
    // The service printed the marker itself; the tool refused nothing.
    let list = AllowedHosts::empty();
    let reason = "service foo failed:\nhost evil.example is not allowed; \
                  allow it for this run with --allow-host evil.example"
        .to_string();
    let state = failed("foo", reason);
    assert_eq!(allow_target(&state, Some("foo"), &list), None);
}

#[test]
fn a_host_the_tool_did_not_refuse_is_never_offered() {
    // The tool refused a.example.org. The text also names b.example.org, which
    // was never refused, so it is not offered.
    let list = AllowedHosts::empty();
    let mut reason = refuse(&list, "foo", &["a.example.org"], Use::Service);
    reason.push_str(
        "\nhost b.example.org is not allowed; allow it for this run with --allow-host b.example.org",
    );
    let state = failed("foo", reason);
    let (_, cands) = allow_target(&state, Some("foo"), &list).unwrap();
    assert_eq!(names(&cands), ["a.example.org"]);
}

#[test]
fn a_single_label_or_a_public_suffix_is_never_offered() {
    let list = AllowedHosts::empty();
    let reason = refuse(&list, "foo", &["com", "co.uk"], Use::Service);
    let state = failed("foo", reason);
    assert_eq!(allow_target(&state, Some("foo"), &list), None);
}

#[test]
fn a_host_refused_for_sources_says_so_and_is_offered() {
    let list = AllowedHosts::empty();
    let reason = refuse(&list, "foo", &["proxy.golang.org"], Use::Source);
    let state = failed("foo", reason);
    let (_, cands) = allow_target(&state, Some("foo"), &list).unwrap();
    assert_eq!(cands[0].askers, [Asker::source("foo")]);
}

#[test]
fn a_no_space_failure_keeps_its_own_retry() {
    let list = AllowedHosts::empty();
    let mut state = TuiState::new(RunScope::default());
    state.pkg_mut("foo").status = PkgStatus::NoSpace;
    assert_eq!(
        retry_label("foo", &state.packages["foo"], &list),
        Some("no space left on device")
    );
}

#[test]
fn keys_at_the_choice_step_allow_only_on_uppercase_y_and_p() {
    let path = PathBuf::from("/cfg/allowed-hosts");
    let mut m = modal_for("foo", &["mirror.example.org"], Some(path));
    // Enter and lowercase `y` do nothing: the prompt stays open, allows nothing.
    assert_eq!(
        allow_host_key(&mut m, KeyCode::Enter),
        AllowHostAction::None
    );
    assert_eq!(
        allow_host_key(&mut m, KeyCode::Char('y')),
        AllowHostAction::None
    );
    assert!(!m.is_finished());
    // The prompt takes no part in the `a` and `A` keys of the list.
    assert_eq!(
        allow_host_key(&mut m, KeyCode::Char('a')),
        AllowHostAction::None
    );
    assert_eq!(
        allow_host_key(&mut m, KeyCode::Char('A')),
        AllowHostAction::None
    );
    assert_eq!(
        allow_host_key(&mut m, KeyCode::Char('Y')),
        AllowHostAction::RunAll(vec!["mirror.example.org".into()])
    );
    let mut n = modal_for("foo", &["mirror.example.org"], None);
    assert_eq!(allow_host_key(&mut n, KeyCode::Esc), AllowHostAction::Close);
}

#[test]
fn a_permanent_allow_needs_the_exact_host_typed() {
    let path = PathBuf::from("/cfg/allowed-hosts");
    let mut m = modal_for("foo", &["mirror.example.org"], Some(path));
    assert_eq!(
        allow_host_key(&mut m, KeyCode::Char('p')),
        AllowHostAction::None
    );
    type_text(&mut m, "mirror.example.com");
    // A wrong name does not confirm, and Enter does nothing until it matches.
    assert_eq!(
        allow_host_key(&mut m, KeyCode::Enter),
        AllowHostAction::None
    );
    assert!(!m.is_finished());
    // Clear the buffer (more backspaces than characters is harmless) and type
    // the name again: the buffer must read exactly the host to confirm.
    for _ in 0..40 {
        assert_eq!(
            allow_host_key(&mut m, KeyCode::Backspace),
            AllowHostAction::None
        );
    }
    type_text(&mut m, "mirror.example.org");
    assert_eq!(
        allow_host_key(&mut m, KeyCode::Enter),
        AllowHostAction::Permanent("mirror.example.org".into())
    );
    assert!(m.is_finished());
}

#[test]
fn several_hosts_are_confirmed_one_at_a_time() {
    let path = PathBuf::from("/cfg/allowed-hosts");
    let mut m = modal_for("foo", &["a.example.org", "b.example.org"], Some(path));
    allow_host_key(&mut m, KeyCode::Char('p'));
    type_text(&mut m, "a.example.org");
    assert_eq!(
        allow_host_key(&mut m, KeyCode::Enter),
        AllowHostAction::Permanent("a.example.org".into())
    );
    assert!(!m.is_finished(), "b.example.org is still to confirm");
    type_text(&mut m, "b.example.org");
    assert_eq!(
        allow_host_key(&mut m, KeyCode::Enter),
        AllowHostAction::Permanent("b.example.org".into())
    );
    assert!(m.is_finished());
}

#[test]
fn without_a_config_directory_p_does_nothing_and_y_allows_for_this_run() {
    let mut m = modal_for("foo", &["mirror.example.org"], None);
    assert_eq!(
        allow_host_key(&mut m, KeyCode::Char('p')),
        AllowHostAction::None
    );
    type_text(&mut m, "mirror.example.org");
    assert_eq!(
        allow_host_key(&mut m, KeyCode::Enter),
        AllowHostAction::None
    );
    assert_eq!(
        allow_host_key(&mut m, KeyCode::Char('Y')),
        AllowHostAction::RunAll(vec!["mirror.example.org".into()])
    );
}

#[test]
fn run_allow_is_logged_and_recorded_only_for_real_changes() {
    let list = AllowedHosts::empty();
    list.allow("github.com", Origin::Run(Via::Flag), None)
        .unwrap();
    let mut state = failed("foo", String::new());
    let added = allow_for_run(&mut state, "foo", &["github.com".into()], &list);
    assert!(added.is_empty(), "already allowed is not added");
    assert!(
        !state
            .run_log
            .iter()
            .any(|l| l.message.contains("allowed github.com")),
        "no log line for a host that was already allowed"
    );
}

#[test]
fn confirm_writes_the_file_privately_and_allows_the_host() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("auto-package").join("allowed-hosts");
    let list = AllowedHosts::empty();
    let mut state = TuiState::new(RunScope::default());
    let mut m = modal_for("foo", &["mirror.example.org"], Some(path.clone()));
    let recorder: RecorderSlot = Arc::new(Mutex::new(None));
    allow_host_key(&mut m, KeyCode::Char('p'));
    type_text(&mut m, "mirror.example.org");
    let action = allow_host_key(&mut m, KeyCode::Enter);
    apply_allow_action(&mut state, &mut m, action, &list, &recorder);
    assert!(list.allows("mirror.example.org", Use::Service));
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(
        text.starts_with("mirror.example.org  # allowed from the TUI for foo"),
        "{text}"
    );
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[test]
fn the_prompt_shows_each_host_on_its_own_line_with_its_scope() {
    let m = modal_for("foo", &["a.example.org", "b.example.org"], None);
    let lines = allow_host_lines(&m);
    assert!(lines.contains(&"  a.example.org".to_string()), "{lines:?}");
    assert!(lines.contains(&"  b.example.org".to_string()), "{lines:?}");
    assert!(lines.iter().any(|l| l.contains("subdomains")), "{lines:?}");
    let mut term = ratatui::Terminal::new(TestBackend::new(200, 30)).unwrap();
    term.draw(|f| {
        let area = f.area();
        render_allow_host(f, &m, area);
    })
    .unwrap();
    let text: String = term
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|c| c.symbol())
        .collect();
    assert!(text.contains("a.example.org"), "{text}");
}

#[test]
fn control_characters_from_a_host_or_a_package_are_not_shown() {
    let m = AllowHostModal::new(
        "foo\x1b[31m".into(),
        vec![Candidate {
            host: "mirror.example.org\x1b[2J".into(),
            askers: vec![Asker::source("foo")],
        }],
        None,
    );
    for line in allow_host_lines(&m) {
        assert!(!line.chars().any(char::is_control), "{line:?}");
    }
}

/// Keeps every event reported to it.
#[derive(Default)]
struct Collect(Mutex<Vec<Event>>);

impl Reporter for Collect {
    fn report(&self, event: &Event) {
        self.0.lock().unwrap().push(event.clone());
    }
}

#[test]
fn a_tui_allow_is_recorded_per_host_when_the_run_has_a_reporter() {
    let sink = Arc::new(Collect::default());
    let slot: RecorderSlot = Arc::new(Mutex::new(None));
    // Before the run has a reporter, nothing is recorded (and nothing panics).
    record_allowed(
        &slot,
        &["a.example.org".into()],
        HostScope::Run,
        HostSource::Tui,
    );
    *slot.lock().unwrap() = Some(sink.clone());
    record_allowed(
        &slot,
        &["a.example.org".into(), "b.example.org".into()],
        HostScope::Persistent,
        HostSource::Tui,
    );
    let got: Vec<(String, HostScope, HostSource)> = sink
        .0
        .lock()
        .unwrap()
        .iter()
        .filter_map(|e| match e {
            Event::HostAllowed {
                host,
                scope,
                source,
            } => Some((host.clone(), *scope, *source)),
            _ => None,
        })
        .collect();
    assert_eq!(
        got,
        [
            (
                "a.example.org".to_string(),
                HostScope::Persistent,
                HostSource::Tui
            ),
            (
                "b.example.org".to_string(),
                HostScope::Persistent,
                HostSource::Tui
            ),
        ]
    );
}

#[test]
fn a_host_refused_for_another_package_is_not_offered_here() {
    let list = AllowedHosts::empty();
    // `bar` was refused the host. `foo`'s failure text names it, as a service
    // printing the refusal would, but `foo` was never refused it.
    let _ = refuse(&list, "bar", &["evil.example.net"], Use::Service);
    let text = format!(
        "403 evil.example.net:443: {}",
        refusal_hint("evil.example.net", Use::Service)
    );
    let mut state = failed("bar", text.clone());
    state.pkg_mut("foo").status = PkgStatus::Failed;
    state.pkg_mut("foo").fail_reason = Some(text);
    assert_eq!(allow_target(&state, Some("foo"), &list), None);
    let (name, cands) = allow_target(&state, Some("bar"), &list).unwrap();
    assert_eq!(name, "bar");
    assert_eq!(names(&cands), ["evil.example.net"]);
}

#[test]
fn a_candidate_says_which_package_and_request_refused_it() {
    let list = AllowedHosts::empty();
    let reason = refuse(&list, "foo", &["mirror.example.org"], Use::Service);
    let state = failed("foo", reason);
    let (_, cands) = allow_target(&state, Some("foo"), &list).unwrap();
    assert_eq!(cands[0].askers, [Asker::service("foo", "cargo_vendor")]);
    let m = AllowHostModal::new("foo".into(), cands, None);
    assert!(
        allow_host_lines(&m)
            .iter()
            .any(|l| l == "      refused for: foo, service cargo_vendor"),
        "{:?}",
        allow_host_lines(&m)
    );
}

#[test]
fn a_prompt_offers_four_hosts_and_says_how_many_wait() {
    let hosts: Vec<String> = (0..5).map(|i| format!("h{i}.example.org")).collect();
    let refs: Vec<&str> = hosts.iter().map(String::as_str).collect();
    let mut m = modal_for("foo", &refs, Some(PathBuf::from("/cfg/allowed-hosts")));
    let lines = allow_host_lines(&m);
    assert_eq!(lines.iter().filter(|l| l.starts_with("  h")).count(), 4);
    assert!(
        !lines.iter().any(|l| l.contains("h4.example.org")),
        "{lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("1 more host is refused")),
        "{lines:?}"
    );
    // `Y` allows the four offered; the fifth is offered next, not allowed now.
    let action = allow_host_key(&mut m, KeyCode::Char('Y'));
    assert_eq!(
        action,
        AllowHostAction::RunAll(hosts[..4].to_vec()),
        "only the offered hosts"
    );
    assert!(!m.is_finished());
    drawn(&m, 200, 30);
    assert!(allow_host_lines(&m).iter().any(|l| l == "  h4.example.org"));
}

#[test]
fn y_is_refused_when_a_host_would_be_cut_off() {
    // Three 253-character hosts: each takes about five rows of an 80x24 terminal.
    let label = |i: usize, len: usize| format!("{i}{}", "a".repeat(len - 1));
    let hosts: Vec<String> = (0..3)
        .map(|i| {
            format!(
                "{}.{}.{}.{}.example.org",
                label(i, 60),
                label(i, 60),
                label(i, 60),
                label(i, 58)
            )
        })
        .collect();
    assert!(hosts.iter().all(|h| h.len() == 253), "{hosts:?}");
    let refs: Vec<&str> = hosts.iter().map(String::as_str).collect();
    let mut m = AllowHostModal::new(
        "foo".into(),
        refs.iter()
            .map(|h| Candidate {
                host: h.to_string(),
                askers: vec![Asker::service("foo", "cargo_vendor")],
            })
            .collect(),
        Some(PathBuf::from("/cfg/allowed-hosts")),
    );
    drawn(&m, 80, 24);
    assert_eq!(
        allow_host_key(&mut m, KeyCode::Char('Y')),
        AllowHostAction::TooSmall
    );
    assert_eq!(
        allow_host_key(&mut m, KeyCode::Char('p')),
        AllowHostAction::TooSmall
    );
    assert!(!m.is_finished());
    // A prompt that was never drawn does not allow either.
    let mut never = modal_for("foo", &["a.example.org"], None);
    never.body.set(None);
    assert_eq!(
        allow_host_key(&mut never, KeyCode::Char('Y')),
        AllowHostAction::TooSmall
    );
}

#[test]
fn two_short_hosts_fit_an_80_by_24_terminal() {
    let mut m = AllowHostModal::new(
        "foo".into(),
        ["a.example.org", "b.example.org"]
            .iter()
            .map(|h| Candidate {
                host: h.to_string(),
                askers: vec![Asker::service("foo", "cargo_vendor")],
            })
            .collect(),
        Some(PathBuf::from(
            "/home/user/.config/auto-package/allowed-hosts",
        )),
    );
    drawn(&m, 80, 24);
    assert!(m.fits());
    assert_eq!(
        allow_host_key(&mut m, KeyCode::Char('Y')),
        AllowHostAction::RunAll(vec!["a.example.org".into(), "b.example.org".into()])
    );
    assert!(m.is_finished());
}

#[test]
fn wrapped_rows_counts_what_the_paragraph_shows() {
    assert_eq!(wrapped_rows("aaa bbb", 7), 1);
    assert_eq!(wrapped_rows("aaa bbb", 6), 2);
    assert_eq!(wrapped_rows(&"x".repeat(25), 10), 3);
    assert_eq!(wrapped_rows("", 10), 1);
    assert_eq!(wrapped_rows("      refused for: foo", 80), 1);
}
