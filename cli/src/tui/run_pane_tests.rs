use super::*;
use ratatui::backend::TestBackend;

/// A run with three packages and a run-level log: the Run pane has something
/// to show, and the table has three rows to want. The scope names no target,
/// so the package names only appear in the table, not the summary line.
fn state_with_run_log() -> TuiState {
    let scope = RunScope {
        project: "proj".to_string(),
        targets: vec![],
    };
    let mut state = TuiState::new(scope);
    for name in ["alpha", "bravo", "charlie"] {
        state.pkg_mut(name);
    }
    for i in 0..20 {
        state.add_global_log(LogLevel::Info, format!("sweep line {i}"));
    }
    state
}

fn screen(terminal: &Terminal<TestBackend>) -> String {
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

fn render_with(state: &TuiState, w: u16, h: u16, browse: &BrowseState) -> String {
    let mut table_state = TableState::default();
    table_state.select(Some(0));
    let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
    terminal
        .draw(|f| {
            render(f, state, &mut table_state, None, browse, None);
        })
        .unwrap();
    screen(&terminal)
}

fn render_at(state: &TuiState, w: u16, h: u16) -> String {
    render_with(state, w, h, &BrowseState::default())
}

#[test]
fn the_run_pane_is_drawn_with_its_title_and_a_log_line_at_small_sizes() {
    let state = state_with_run_log();
    for (w, h) in [(80u16, 24u16), (60, 16), (50, 12), (40, 10), (30, 8)] {
        let text = render_at(&state, w, h);
        assert!(text.contains("╭Run"), "{w}x{h} has no Run pane:\n{text}");
        assert!(
            text.contains("sweep line 19"),
            "{w}x{h} shows no run log line:\n{text}"
        );
    }
}

#[test]
fn every_package_is_on_screen_at_80x24_beside_the_run_log() {
    let text = render_at(&state_with_run_log(), 80, 24);
    for name in ["alpha", "bravo", "charlie"] {
        assert!(text.contains(name), "{name} is not on screen:\n{text}");
    }
}

#[test]
fn a_package_row_is_on_screen_at_24x10_beside_the_run_log() {
    let text = render_at(&state_with_run_log(), 24, 10);
    assert!(text.contains("alpha"), "no package row:\n{text}");
    assert!(text.contains("╭Run"), "no Run pane:\n{text}");
}

#[test]
fn no_size_from_1x1_to_82x32_panics_with_a_run_log() {
    let state = state_with_run_log();
    for w in 1..=82u16 {
        for h in 1..=32u16 {
            for focus in [Focus::Table, Focus::Log, Focus::Activity] {
                for expanded in [false, true] {
                    for input in [None, Some("abc")] {
                        let browse = BrowseState {
                            focus,
                            act_expanded: expanded,
                            ..BrowseState::default()
                        };
                        let mut table_state = TableState::default();
                        table_state.select(Some(0));
                        let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
                        terminal
                            .draw(|f| {
                                render(f, &state, &mut table_state, input, &browse, None);
                            })
                            .unwrap();
                    }
                }
            }
        }
    }
}
