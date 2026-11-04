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
    /// The `%if` directive, whitespace collapsed.
    text: String,
    /// The `%elif` directives read so far, whitespace collapsed.
    elifs: Vec<String>,
    /// The macros the conditions name, with the values the spec gives them, so
    /// a guard changes when its input does (`[use_patches=1]`).
    inputs: Vec<String>,
    /// The branch the frame is in.
    branch: Branch,
}

#[derive(Clone, Copy, PartialEq)]
enum Branch {
    Then,
    Elif,
    Else,
}

impl Frame {
    fn new(state: Option<bool>, line: &str, macros: &HashMap<String, String>) -> Self {
        Frame {
            inputs: macro_inputs(line, macros),
            state,
            prior: Taken::No,
            text: collapse(line),
            elifs: Vec::new(),
            branch: Branch::Then,
        }
    }

    /// The branch as a guard names it: `%if X [then]`, `%if X [elif 1: %elif Y]`
    /// or `%if X [else after %elif Y]`.
    fn guard(&self) -> String {
        let branch = match self.branch {
            Branch::Then => "then".to_string(),
            Branch::Elif => format!(
                "elif {}: {}",
                self.elifs.len(),
                self.elifs.last().map_or("", String::as_str)
            ),
            Branch::Else if self.elifs.is_empty() => "else".to_string(),
            Branch::Else => format!("else after {}", self.elifs.join(", ")),
        };
        let inputs: String = self.inputs.iter().map(|i| format!(" {{{i}}}")).collect();
        format!("{} [{branch}]{inputs}", self.text)
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
                self.frames.push(Frame::new(state, trimmed, macros));
            }
            "%ifarch" | "%ifnarch" | "%ifos" | "%ifnos" => {
                self.frames.push(Frame::new(None, trimmed, macros))
            }
            "%elif" | "%elifarch" | "%elifnarch" | "%elifos" | "%elifnos" => {
                if let Some(frame) = self.frames.last_mut() {
                    frame.advance();
                    frame.branch = Branch::Elif;
                    frame.elifs.push(collapse(trimmed));
                    for input in macro_inputs(trimmed, macros) {
                        if !frame.inputs.contains(&input) {
                            frame.inputs.push(input);
                        }
                    }
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
                    frame.branch = Branch::Else;
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

    /// The open frames that are not known to be taken, outermost first, each
    /// as `%if X [then]`, `%if X [elif 1: %elif Y]` or `%if X [else after
    /// %elif Y]` (whitespace collapsed).
    pub(crate) fn guards(&self) -> Vec<String> {
        self.frames
            .iter()
            .filter(|f| f.state != Some(true))
            .map(Frame::guard)
            .collect()
    }
}

/// `text` with each run of whitespace made one space.
pub(crate) fn collapse(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `name=value` for each macro a condition line names (`%{x}`, `%x`, `%{?x}`,
/// `%{!?x:..}`), the value as the spec defines it or `undefined`. The `with`
/// and `without` bcond tests name no macro.
fn macro_inputs(line: &str, macros: &HashMap<String, String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    // The directive itself (`%if`, `%elifarch`) names no macro.
    let mut rest = line
        .trim_start()
        .split_once(char::is_whitespace)
        .map_or("", |(_, r)| r);
    while let Some(pos) = rest.find('%') {
        let after = rest[pos + 1..].trim_start_matches('{');
        let after = after.trim_start_matches(['!', '?']);
        let name: String = after
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        let bcond = matches!(name.as_str(), "with" | "without");
        if !name.is_empty() && !bcond {
            let value = macros.get(&name).map_or("undefined", String::as_str);
            let input = format!("{name}={value}");
            if !out.contains(&input) {
                out.push(input);
            }
        }
        rest = &rest[pos + 1..];
    }
    out
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
