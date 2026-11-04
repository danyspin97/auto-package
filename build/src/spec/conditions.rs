//! The `%if` frames of a spec: which lines a reader of the spec text may
//! take as present. A condition is decided only when it is a plain integer
//! (optionally with `%{?name}` taken from the spec's own defines); every
//! other one is unknown, never guessed.

use std::collections::HashMap;

/// Whether an earlier branch of the current `%if` chain was taken.
#[derive(Clone, Copy, PartialEq)]
enum Taken {
    Yes,
    No,
    Maybe,
}

struct Frame {
    /// `Some(true)` taken, `Some(false)` skipped, `None` cannot be evaluated.
    state: Option<bool>,
    prior: Taken,
}

impl Frame {
    fn new(state: Option<bool>) -> Self {
        Frame {
            state,
            prior: Taken::No,
        }
    }

    /// Fold the branch just left into `prior` before moving to the next one.
    fn advance(&mut self) {
        self.prior = match (self.prior, self.state) {
            (Taken::Yes, _) | (_, Some(true)) => Taken::Yes,
            (Taken::Maybe, _) | (_, None) => Taken::Maybe,
            _ => Taken::No,
        };
    }
}

/// The open `%if` frames while a spec is read line by line.
pub(crate) struct CondStack {
    frames: Vec<Frame>,
}

impl CondStack {
    pub(crate) fn new() -> Self {
        CondStack { frames: Vec::new() }
    }

    /// Read one line. True when it was `%if*`, `%elif`, `%else` or `%endif`:
    /// the caller then skips it.
    pub(crate) fn feed(&mut self, line: &str, macros: &HashMap<String, String>) -> bool {
        let trimmed = line.trim_start();
        let word = trimmed.split_whitespace().next().unwrap_or("");
        let rest = trimmed[word.len()..].trim();
        match word {
            "%if" => {
                let state = eval_condition(rest, macros);
                self.frames.push(Frame::new(state));
            }
            "%ifarch" | "%ifnarch" | "%ifos" | "%ifnos" => self.frames.push(Frame::new(None)),
            "%elif" | "%elifarch" | "%elifnarch" | "%elifos" | "%elifnos" => {
                if let Some(frame) = self.frames.last_mut() {
                    frame.advance();
                    frame.state = match frame.prior {
                        Taken::No if word == "%elif" => eval_condition(rest, macros),
                        Taken::Yes => Some(false),
                        _ => None,
                    };
                }
            }
            "%else" => {
                if let Some(frame) = self.frames.last_mut() {
                    frame.advance();
                    frame.state = match frame.prior {
                        Taken::Yes => Some(false),
                        Taken::Maybe => None,
                        Taken::No => Some(true),
                    };
                }
            }
            "%endif" => {
                self.frames.pop();
            }
            _ => return false,
        }
        true
    }

    /// `Some(true)`: every open frame is taken. `Some(false)`: one is skipped.
    /// `None`: none is skipped, but one cannot be evaluated.
    pub(crate) fn taken(&self) -> Option<bool> {
        if self.frames.iter().any(|f| f.state == Some(false)) {
            Some(false)
        } else if self.frames.iter().any(|f| f.state.is_none()) {
            None
        } else {
            Some(true)
        }
    }
}

/// Evaluate the simple `%if` shapes: an integer literal, optionally with a
/// `%{?name}` expansion (`0%{?name}`). A macro the spec itself never defined
/// is unknown, not false: rpm's own config may define it.
fn eval_condition(cond: &str, macros: &HashMap<String, String>) -> Option<bool> {
    let cond = cond.trim();
    let mut expanded = String::new();
    let mut rest = cond;
    while let Some(pos) = rest.find("%{?") {
        expanded.push_str(&rest[..pos]);
        let after = &rest[pos + 3..];
        let end = after.find('}')?;
        let name = &after[..end];
        if name.is_empty() || !name.chars().all(|c| c.is_alphanumeric() || c == '_') {
            return None;
        }
        expanded.push_str(macros.get(name)?);
        rest = &after[end + 1..];
    }
    expanded.push_str(rest);
    let n: i64 = expanded.trim().parse().ok()?;
    Some(n != 0)
}
