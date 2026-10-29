use super::*;

/// Feeds a recorded run (`AP_REPLAY=<run.jsonl>`) through the table state and
/// prints what the screen would list. Ignored: it needs a recorded run.
#[test]
#[ignore]
fn replay_a_recorded_run() {
    let Ok(path) = std::env::var("AP_REPLAY") else {
        return;
    };
    let text = std::fs::read_to_string(path).unwrap();
    let mut state = TuiState::new(RunScope::default());
    let mut skipped = 0;
    for line in text.lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        let ambient = value
            .get("pkg")
            .and_then(|p| p.as_str())
            .map(str::to_string);
        match serde_json::from_value::<Event>(value) {
            Ok(event) => state.apply(Msg {
                pkg: ambient,
                event,
            }),
            Err(_) => skipped += 1,
        }
    }
    eprintln!("rows: {} (skipped {skipped} lines)", state.order.len());
    let mut seen = std::collections::HashSet::new();
    for n in &state.order {
        if !seen.insert(n.clone()) {
            eprintln!("DUPLICATE ROW {n}");
        }
    }
    eprintln!("order: {:?}", state.order);
    eprintln!("queued: {:#?}", queued_lines(&state));
    eprintln!("built: {}", built_lines(&state).len());
}
