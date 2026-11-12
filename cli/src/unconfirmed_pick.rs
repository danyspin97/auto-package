//! Choosing among the repositories a search found for a bare name, on a
//! terminal, when there is no single clear one to confirm with `[y/N]`.

use ap_build::pipeline::Candidate;

use crate::unconfirmed_offer::Terminal;

/// Asks that are made before giving up on unreadable answers.
const ASKS: usize = 3;

/// The candidate the person picks, if any. Nothing is asked without a
/// terminal to ask on (or with no candidates). `show` prints the numbered
/// table, `ask` shows the prompt and returns the line read. Enter, `n`, `no`,
/// `q` or `quit` (any case), or end of input picks nothing at once. So does
/// the third answer that is not a listed number. A leading `+` is not a number.
pub(crate) fn pick<'a>(
    name: &str,
    candidates: &'a [Candidate],
    term: Terminal,
    (width, colour): (usize, bool),
    show: &dyn Fn(&str),
    ask: &dyn Fn(&str) -> Option<String>,
) -> Option<&'a Candidate> {
    if !term.can_ask() || candidates.is_empty() {
        return None;
    }
    show(&crate::unconfirmed::numbered_table(
        name, candidates, width, colour,
    ));
    let range = match candidates.len() {
        1 => "1".to_string(),
        n => format!("1-{n}"),
    };
    let prompt = format!("Build which? [{range}, Enter to cancel] ");
    for _ in 0..ASKS {
        let answer = ask(&prompt)?;
        let answer = answer.trim();
        let lower = answer.to_ascii_lowercase();
        if matches!(lower.as_str(), "" | "n" | "no" | "q" | "quit") {
            return None;
        }
        let number = if answer.starts_with('+') {
            None
        } else {
            answer.parse::<usize>().ok()
        };
        if let Some(c) = number.and_then(|n| candidates.get(n.checked_sub(1)?)) {
            return Some(c);
        }
        show(&format!("not a number from {range}: {answer}"));
    }
    None
}

#[cfg(test)]
#[path = "unconfirmed_pick_tests.rs"]
mod tests;
