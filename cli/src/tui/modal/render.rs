use super::*;

pub(super) fn spinner(since: Instant) -> &'static str {
    const FRAMES: [&str; 10] = [
        "\u{280b}", "\u{2819}", "\u{2839}", "\u{2838}", "\u{283c}", "\u{2834}", "\u{2826}",
        "\u{2827}", "\u{2807}", "\u{280f}",
    ];
    FRAMES[(since.elapsed().as_millis() / 100 % 10) as usize]
}

pub(super) fn centered(area: Rect, w_pct: u16, h_pct: u16) -> Rect {
    let w = area.width * w_pct / 100;
    let h = area.height * h_pct / 100;
    Rect {
        x: area.x + (area.width - w) / 2,
        y: area.y + (area.height - h) / 2,
        width: w,
        height: h,
    }
}

pub fn render_modal(f: &mut Frame, state: &TuiState, modal: &Modal, area: Rect) {
    match modal {
        Modal::Diff(v) => render_diff(f, state, v, area),
        Modal::Submit(m) => render_submit(f, m, centered(area, 92, 86)),
        Modal::Explain(v) => render_explain(f, v, area),
        Modal::AllowHost(m) => render_allow_host(f, m, centered(area, 90, 90)),
    }
}

pub(super) fn hint_line(text: &str, width: usize) -> Line<'static> {
    Line::from(Span::styled(
        truncate_with_ellipsis(text, width),
        Style::default().fg(Color::DarkGray),
    ))
}
