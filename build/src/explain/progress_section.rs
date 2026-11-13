//! The progress lines of a package, as `explain` lists them after the warnings.

use super::model::PROGRESS_LINES;
use super::render::Out;
use super::*;

/// The "Routine actions and progress" section: what the run did on purpose or
/// went on to fix. Nothing is printed when there are none.
pub(super) fn progress_section(o: &mut Out, progress: &[WarningLine]) {
    if progress.is_empty() {
        return;
    }
    o.heading("Routine actions and progress");
    for w in progress.iter().take(PROGRESS_LINES) {
        let mut t = String::new();
        if let Some(st) = w.step {
            let _ = write!(t, "[step {st}] ");
        }
        t.push_str(&w.message);
        if w.count > 1 {
            let _ = write!(t, " (x{})", w.count);
        }
        o.para(2, "- ", Kind::Plain, &t);
    }
    if progress.len() > PROGRESS_LINES {
        o.para(
            2,
            "",
            Kind::Plain,
            &format!(
                "... {} more line(s) in run.jsonl",
                progress.len() - PROGRESS_LINES
            ),
        );
    }
}
