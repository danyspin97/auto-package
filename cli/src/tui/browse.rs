use super::*;

pub(super) fn select_next(selected: &mut Option<String>, state: &TuiState) {
    let visible = state.visible_order();
    if visible.is_empty() {
        *selected = None;
        return;
    }
    let i = selected
        .as_deref()
        .and_then(|s| visible.iter().position(|n| n.as_str() == s))
        .unwrap_or(0);
    *selected = Some(visible[(i + 1).min(visible.len() - 1)].clone());
}

pub(super) fn select_prev(selected: &mut Option<String>, state: &TuiState) {
    let visible = state.visible_order();
    if visible.is_empty() {
        *selected = None;
        return;
    }
    let i = selected
        .as_deref()
        .and_then(|s| visible.iter().position(|n| n.as_str() == s))
        .unwrap_or(0);
    *selected = Some(visible[i.saturating_sub(1)].clone());
}

/// Which pane the arrow keys and paging act on. Tab cycles it.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub(super) enum Focus {
    #[default]
    Table,
    Log,
    Activity,
}

/// Scroll state for the selected package's panes, kept by the event loop
/// across frames. Offsets are clamped against the previous frame's sizes.
#[derive(Default)]
pub(super) struct BrowseState {
    pub(super) focus: Focus,
    /// Lines above the tail of the Event Log; 0 follows the newest line.
    pub(super) log_off: usize,
    /// `PkgState::log_seq` at the last frame, to count the lines that arrived.
    pub(super) log_seen: u64,
    /// Selected Agent Activity entry, an index into the package's list.
    /// `usize::MAX` means the newest entry, so the selection keeps following
    /// as entries arrive until the user moves it.
    pub(super) act_sel: usize,
    /// First Agent Activity entry shown in the list.
    pub(super) act_top: usize,
    /// Enter shows the selected entry's full text in place of the Event Log.
    pub(super) act_expanded: bool,
    /// First wrapped line shown of the expanded text.
    pub(super) detail_off: usize,
    /// Applied Event Log filter: only lines containing it are shown.
    pub(super) filter: Option<String>,
    /// The `/` prompt while it is open, with the text typed so far.
    pub(super) filter_input: Option<String>,
    /// `l` replaces the detail panes with the Queued and Built lists.
    pub(super) show_lists: bool,
    /// First line shown of each list; separate because the lists differ in
    /// length and a shared offset would end the shorter one early.
    pub(super) queued_off: usize,
    pub(super) built_off: usize,
    /// The diff viewer or submit confirmation, while one is open.
    pub(super) modal: Option<Modal>,
    pub(super) diff_seq: u64,
}

impl BrowseState {
    pub(super) fn next_seq(&mut self) -> u64 {
        self.diff_seq += 1;
        self.diff_seq
    }

    /// Start over for a newly selected package: another package's offsets
    /// mean nothing here.
    pub(super) fn reset_for_package(&mut self, log_seq: u64) {
        self.log_off = 0;
        self.log_seen = log_seq;
        self.act_sel = usize::MAX;
        self.act_top = 0;
        self.act_expanded = false;
        self.detail_off = 0;
        self.filter = None;
        self.filter_input = None;
    }
}

/// What one frame learned about the pane sizes, handed back to the event loop
/// so key handling clamps against what is actually on screen.
#[derive(Default, Clone, Copy)]
pub(super) struct RenderFeedback {
    pub(super) log_total: usize,
    pub(super) log_viewport: usize,
    /// The log offset after clamping to this frame's sizes.
    pub(super) log_off: usize,
    pub(super) act_len: usize,
    pub(super) act_viewport: usize,
    /// The Agent Activity top after keeping the selection on screen.
    pub(super) act_top: usize,
    /// Wrapped lines of the expanded text, and how many of them fit.
    pub(super) detail_total: usize,
    pub(super) detail_viewport: usize,
    pub(super) detail_off: usize,
    /// Length of each list and how many lines fit, for scrolling them.
    pub(super) queued_total: usize,
    pub(super) built_total: usize,
    pub(super) lists_viewport: usize,
}

/// The pane after (or, with `back`, before) `f`, wrapping around.
pub(super) fn cycle_focus(f: Focus, back: bool) -> Focus {
    match (f, back) {
        (Focus::Table, false) | (Focus::Activity, true) => Focus::Log,
        (Focus::Log, false) | (Focus::Table, true) => Focus::Activity,
        (Focus::Activity, false) | (Focus::Log, true) => Focus::Table,
    }
}

/// Rows inside a bordered pane; the top and bottom border take two.
pub(super) fn viewport_height(area_h: u16) -> usize {
    area_h.saturating_sub(2) as usize
}

/// How far a view can scroll back from the tail before its top is reached.
pub(super) fn max_offset(total: usize, viewport: usize) -> usize {
    total.saturating_sub(viewport)
}

pub(super) fn clamp_offset(off: usize, total: usize, viewport: usize) -> usize {
    off.min(max_offset(total, viewport))
}

/// The lines a view shows when it is `from_bottom` lines above the tail.
/// The offset is clamped first, so content shrinking under it cannot make
/// the range underflow.
pub(super) fn window(total: usize, viewport: usize, from_bottom: usize) -> Range<usize> {
    let off = clamp_offset(from_bottom, total, viewport);
    let end = total - off;
    let start = end.saturating_sub(viewport);
    start..end
}

/// New scroll offset for a log view counted from the tail (0 = following).
/// Paging moves by one screen less a line, so the last line stays in view.
pub(super) fn scroll_key(code: KeyCode, off: usize, total: usize, viewport: usize) -> usize {
    let page = viewport.saturating_sub(1).max(1);
    let next = match code {
        KeyCode::Up | KeyCode::Char('k') => off.saturating_add(1),
        KeyCode::Down | KeyCode::Char('j') => off.saturating_sub(1),
        KeyCode::PageUp => off.saturating_add(page),
        KeyCode::PageDown => off.saturating_sub(page),
        KeyCode::Home => max_offset(total, viewport),
        KeyCode::End => 0,
        _ => off,
    };
    clamp_offset(next, total, viewport)
}

/// Keeps a tail-relative view still while lines arrive. A view at the tail
/// keeps following; a scrolled one moves up by the number of new lines so it
/// shows the same content. Eviction from the front is handled by the caller's
/// clamp.
pub(super) fn follow_adjust(off: usize, new_seq: u64, old_seq: u64) -> usize {
    if off == 0 {
        return 0;
    }
    let arrived = usize::try_from(new_seq.saturating_sub(old_seq)).unwrap_or(usize::MAX);
    off.saturating_add(arrived)
}

/// Case-insensitive substring match. An empty needle matches everything.
#[cfg_attr(not(test), allow(dead_code))]
pub(super) fn filter_matches(line: &str, needle: &str) -> bool {
    needle.is_empty() || line.to_lowercase().contains(&needle.to_lowercase())
}

/// Indices of the log lines that pass `filter_matches`, in order.
#[cfg_attr(not(test), allow(dead_code))]
pub(super) fn filtered_indices<'a>(
    lines: impl IntoIterator<Item = &'a LogLine>,
    needle: &str,
) -> Vec<usize> {
    lines
        .into_iter()
        .enumerate()
        .filter(|(_, l)| filter_matches(&l.message, needle))
        .map(|(i, _)| i)
        .collect()
}

/// New selection in the Agent Activity list (index into the entries, 0 = oldest).
pub(super) fn activity_move(sel: usize, total: usize, code: KeyCode, viewport: usize) -> usize {
    if total == 0 {
        return 0;
    }
    let last = total - 1;
    let sel = sel.min(last);
    let page = viewport.saturating_sub(1).max(1);
    match code {
        KeyCode::Up | KeyCode::Char('k') => sel.saturating_sub(1),
        KeyCode::Down | KeyCode::Char('j') => (sel + 1).min(last),
        KeyCode::PageUp => sel.saturating_sub(page),
        KeyCode::PageDown => (sel + page).min(last),
        KeyCode::Home => 0,
        KeyCode::End => last,
        _ => sel,
    }
}

/// First visible entry after keeping `sel` on screen with the smallest scroll
/// that does it.
pub(super) fn activity_top(sel: usize, top: usize, total: usize, viewport: usize) -> usize {
    if total == 0 || viewport == 0 {
        return 0;
    }
    let sel = sel.min(total - 1);
    let top = clamp_offset(top, total, viewport);
    let top = if sel < top {
        sel
    } else if sel >= top + viewport {
        sel + 1 - viewport
    } else {
        top
    };
    clamp_offset(top, total, viewport)
}

/// Like `scroll_key`, but for text read from the top: `off` is the first
/// visible line, so Down moves towards the end.
pub(super) fn text_scroll_key(code: KeyCode, off: usize, total: usize, viewport: usize) -> usize {
    let page = viewport.saturating_sub(1).max(1);
    let next = match code {
        KeyCode::Up | KeyCode::Char('k') => off.saturating_sub(1),
        KeyCode::Down | KeyCode::Char('j') => off.saturating_add(1),
        KeyCode::PageUp => off.saturating_sub(page),
        KeyCode::PageDown => off.saturating_add(page),
        KeyCode::Home => 0,
        KeyCode::End => max_offset(total, viewport),
        _ => off,
    };
    clamp_offset(next, total, viewport)
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum EscAction {
    /// Cancel the `/` prompt, keeping any applied filter.
    ClosePrompt,
    ClearFilter,
    /// Close the expanded entry.
    Collapse,
    Quit,
}

/// What Esc does. Transient UI is dismissed before anything quits, in this
/// order, so Esc only ends the run once nothing else is open.
pub(super) fn esc_action(browse: &BrowseState) -> EscAction {
    if browse.filter_input.is_some() {
        EscAction::ClosePrompt
    } else if browse.filter.is_some() {
        EscAction::ClearFilter
    } else if browse.act_expanded {
        EscAction::Collapse
    } else {
        EscAction::Quit
    }
}
