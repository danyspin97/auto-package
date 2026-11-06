use super::*;
use ratatui::backend::TestBackend;

fn screen(t: &Terminal<TestBackend>) -> String {
    (0..t.backend().buffer().area.height)
        .map(|y| row_text(t, y))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn a_short_terminal_keeps_a_package_row_and_one_readable_pane() {
    let state = state_with_history();
    let narrow = screen(&render_at(&state, 24, 10));
    assert!(narrow.contains("foo"), "{narrow}");
    assert!(narrow.contains("Event Log") && !narrow.contains("Agent Activity"));
    assert!(narrow.contains("log line"), "{narrow}");
    let wide = screen(&render_at(&state, 80, 10));
    assert!(wide.contains("foo") && wide.contains("log line"), "{wide}");
    assert!(wide.contains("Agent Activity"));
}

#[test]
fn the_too_small_notice_gives_the_size() {
    let terminal = render_at(&state_with_history(), 40, 5);
    assert!(row_text(&terminal, 0).contains("(40x5, need 20x6)"));
}

fn state_with_history() -> TuiState {
    let scope = RunScope {
        project: "home:foo:bar".to_string(),
        targets: vec!["foo".to_string(), "bar".to_string(), "baz".to_string()],
        jobs: 4,
    };
    let mut state = TuiState::new(scope);
    let pkg = state.pkg_mut("foo");
    for i in 0..50 {
        pkg.add_log(LogLevel::Info, format!("log line {i} with some text"));
    }
    for i in 0..30 {
        pkg.add_agent_activity(ActivityKind::ToolUse, format!("→ Read {i}\nsecond line"));
    }
    pkg.add_agent_activity(
        ActivityKind::Reply,
        format!("done (confidence: high)\n{}", "long reasoning ".repeat(40)),
    );
    state
}

#[test]
fn every_focus_and_size_renders_without_panicking() {
    let state = state_with_history();
    for (w, h) in [
        (80u16, 30u16),
        (80, 24),
        (60, 15),
        (40, 10),
        (20, 6),
        (20, 8),
        (10, 3),
        (3, 3),
    ] {
        for focus in [Focus::Table, Focus::Log, Focus::Activity] {
            for expanded in [false, true] {
                for filter in [None, Some("line 4".to_string())] {
                    for act_sel in [0, usize::MAX] {
                        for log_off in [0, 1000] {
                            for input in [None, Some("abc")] {
                                let browse = BrowseState {
                                    focus,
                                    act_expanded: expanded,
                                    filter: filter.clone(),
                                    act_sel,
                                    log_off,
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
    }
}

#[test]
fn an_empty_run_renders_without_panicking() {
    let state = TuiState::new(RunScope::default());
    let browse = BrowseState {
        focus: Focus::Activity,
        act_expanded: true,
        act_sel: usize::MAX,
        ..BrowseState::default()
    };
    let mut table_state = TableState::default();
    let mut terminal = Terminal::new(TestBackend::new(80, 30)).unwrap();
    terminal
        .draw(|f| {
            render(f, &state, &mut table_state, None, &browse, None);
        })
        .unwrap();
}

fn row_text(terminal: &Terminal<TestBackend>, y: u16) -> String {
    let buf = terminal.backend().buffer();
    (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect()
}

fn render_at(state: &TuiState, w: u16, h: u16) -> Terminal<TestBackend> {
    let browse = BrowseState::default();
    let mut table_state = TableState::default();
    table_state.select(Some(0));
    let mut terminal = Terminal::new(TestBackend::new(w, h)).unwrap();
    terminal
        .draw(|f| {
            render(f, state, &mut table_state, None, &browse, None);
        })
        .unwrap();
    terminal
}

#[test]
fn the_header_names_the_project_and_targets_when_there_is_room() {
    let terminal = render_at(&state_with_history(), 200, 30);
    let row = row_text(&terminal, 1);
    assert!(
        row.contains("project: home:foo:bar | target: foo, bar, baz │ queued 1 (4 at a time)"),
        "{row}"
    );
    // Narrower: the targets give way, the project and the counters stay.
    let row = row_text(&terminal_at(80), 1);
    assert!(
        row.contains("project: home:foo:bar │ queued 1 · run 0 · done 0"),
        "{row}"
    );
}

fn terminal_at(width: u16) -> Terminal<TestBackend> {
    render_at(&state_with_history(), width, 30)
}

#[test]
fn a_terminal_below_the_minimum_gets_one_notice_line() {
    let state = state_with_history();
    let terminal = render_at(&state, 19, 10);
    assert!(row_text(&terminal, 0).starts_with("terminal too small"));
    assert!(!row_text(&terminal, 1).contains("Event Log"));
    // Cut to the width: "termina..." is all that fits in 10 columns.
    let terminal = render_at(&state, 10, 3);
    assert_eq!(row_text(&terminal, 0).trim_end(), "termina...");
}

#[test]
fn the_minimum_size_is_drawn_in_full() {
    let state = state_with_history();
    let terminal = render_at(&state, 20, 6);
    assert!(!row_text(&terminal, 0).contains("terminal too small"));
}

#[test]
fn too_small_is_width_under_20_or_height_under_6() {
    assert!(too_small(Rect::new(0, 0, 19, 30)));
    assert!(too_small(Rect::new(0, 0, 80, 5)));
    assert!(!too_small(Rect::new(0, 0, 20, 6)));
}

#[test]
fn short_terminals_get_bare_summary_and_footer_lines() {
    assert_eq!(stacked_heights(30, 5, false), [3, 5, 19, 0, 3]);
    assert_eq!(stacked_heights(19, 5, false), [1, 5, 12, 0, 1]);
    assert_eq!(stacked_heights(6, 5, false), [1, 2, 2, 0, 1]);
}

#[test]
fn stacked_heights_always_fill_the_terminal() {
    for height in 6..=80u16 {
        for wanted in 5..=12u16 {
            for has_run in [false, true] {
                let [summary, table, detail, run, footer] =
                    stacked_heights(height, wanted, has_run);
                assert_eq!(summary + table + detail + run + footer, height);
                assert!(table <= wanted);
            }
        }
    }
}

#[test]
fn the_table_keeps_the_most_important_columns_when_narrow() {
    // From 91 columns every column fits with the full status.
    assert_eq!(visible_columns(91), vec![0, 1, 2, 3, 4, 5]);
    assert_eq!(column_width(0, 91), Some(29));
    // Narrower, the status is cut down so no column is lost for it.
    assert_eq!(visible_columns(90), vec![0, 1, 2, 3, 4, 5]);
    assert_eq!(column_width(0, 90), Some(12));
    assert_eq!(visible_columns(60), vec![0, 1, 2, 3]);
    assert_eq!(visible_columns(40), vec![0, 1, 3]);
    // The package name is always there, even when nothing else fits.
    assert_eq!(visible_columns(5), vec![1]);
}

#[test]
fn a_narrow_table_drops_cost_and_keeps_the_package_name() {
    let state = state_with_history();
    let terminal = render_at(&state, 40, 24);
    // Summary takes rows 0-2, so the table header sits on row 4.
    let header = row_text(&terminal, 4);
    assert!(header.contains("Package"));
    assert!(!header.contains("Cost"));
}

#[test]
fn the_run_pane_is_sized_only_with_a_log() {
    assert_eq!(stacked_heights(40, 5, false)[3], 0);
    let [_, _, _, run, _] = stacked_heights(40, 5, true);
    assert!((RUN_PANE_MIN_HEIGHT..=RUN_PANE_MAX_HEIGHT).contains(&run));
    assert_eq!(stacked_heights(24, 5, true), [3, 5, 9, 4, 3]);
}

#[test]
fn without_a_log_the_table_is_sized_as_before() {
    for height in 6..=80u16 {
        for wanted in 4..=12u16 {
            let chrome = if height >= FULL_CHROME_MIN_HEIGHT {
                3
            } else {
                1
            };
            let body = height - 2 * chrome;
            let cap = if body >= 24 {
                body / 2
            } else {
                (body / 3).max(4.min(body / 2))
            };
            assert_eq!(stacked_heights(height, wanted, false)[1], wanted.min(cap));
        }
    }
}

#[test]
fn a_run_log_keeps_a_package_row_from_14_rows_down() {
    for height in 14..=80u16 {
        for wanted in 4..=12u16 {
            let [_, table, _, _, _] = stacked_heights(height, wanted, true);
            assert!(table >= 4.min(wanted), "{height} rows, {wanted} wanted");
        }
    }
}

#[test]
fn panes_stack_when_narrow_and_sit_side_by_side_when_wide() {
    let wide = two_panes(Rect::new(0, 0, 80, 10), 65);
    assert_eq!(wide[0].y, wide[1].y);
    assert_eq!(wide[0].width + wide[1].width, 80);
    assert_eq!(wide[0].width, 52);

    let narrow = two_panes(Rect::new(0, 0, 40, 10), 65);
    assert_eq!(narrow[0].x, narrow[1].x);
    assert_eq!(narrow[0].width, 40);
    assert_eq!(narrow[0].height + narrow[1].height, 10);
}

#[test]
fn first_fitting_takes_the_first_option_that_fits_else_the_last() {
    let options = vec!["abcdef", "abc", "a"];
    assert_eq!(
        first_fitting(options.clone(), 6, |s| s.len()),
        Some("abcdef")
    );
    assert_eq!(first_fitting(options.clone(), 3, |s| s.len()), Some("abc"));
    assert_eq!(first_fitting(options.clone(), 0, |s| s.len()), Some("a"));
    assert_eq!(first_fitting(Vec::<&str>::new(), 5, |s| s.len()), None);
}

#[test]
fn suffix_keeps_the_end_of_the_text_within_its_width() {
    assert_eq!(suffix_within_width("abcdef", 3), "def");
    assert_eq!(suffix_within_width("abc", 10), "abc");
    assert_eq!(suffix_within_width("abc", 0), "");
    assert_eq!(suffix_within_width("日本", 3), "本");
}
