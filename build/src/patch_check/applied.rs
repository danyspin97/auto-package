//! Whether a spec applies a patch, read from the spec text. The reader walks
//! the `%if` frames and the `%prep` lines once and gives, for each declared
//! patch, the guards under which a line of `%prep` applies it. An edit that
//! keeps a patch declared but stops applying it everywhere it was applied is
//! a disable ([`disabled_patches`]).
//!
//! The reader never calls a condition true unless the spec's own text proves
//! it (see `CondStack`; a `%{with x}` of a bcond the spec declares is read
//! at its default): an unknown guard is kept as text, and the
//! comparison treats a changed or new one as a change. A patch the before
//! spec applies through a construct the reader does not know (a custom macro
//! in `%prep`) has no application and is never reported as disabled.
//!
//! A line whose first non-blank character is `#` applies nothing, though rpm
//! expands a macro in a comment. A commented-out application is read as a
//! disable. A spec that really applies a patch through a macro in a comment
//! reads as not applying it, in both specs of a comparison, so that patch is
//! never reported as disabled.

use crate::spec::{CondStack, collapse, section_word};
use std::collections::{BTreeMap, BTreeSet};

/// Guards under which one application of a patch runs (empty: always).
pub type GuardPath = Vec<String>;

/// Why a patch an edit keeps declared is no longer applied as it was.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Disabled {
    /// Still declared, but no line of `%prep` applies it any more.
    NotApplied,
    /// Its `PatchN:` line is under a condition that is always false.
    NotDeclared { guard: String },
    /// Applied only under a guard it did not have before.
    Conditional { guard: String },
}

/// The patch numbers an application line applies.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Applies {
    All,
    Numbers(Vec<String>),
    /// `%autopatch -m A -M B`: the numbers in `[A, B]`, a missing bound open.
    Range(Option<u64>, Option<u64>),
}

struct Application {
    applies: Applies,
    guards: Vec<String>,
}

struct Declaration {
    number: String,
    name: String,
    /// The guards of the frames the line is in.
    guards: Vec<String>,
    /// The line is under a frame that is always false: not declared.
    skipped: bool,
}

struct Scan {
    declarations: Vec<Declaration>,
    applications: Vec<Application>,
}

/// The options of `%patch` that take a value as the next word.
const PATCH_VALUE_OPTIONS: &[&str] = &["-p", "-F", "-d", "-o", "-z", "-b", "-P"];

fn is_number(text: &str) -> bool {
    !text.is_empty() && text.chars().all(|c| c.is_ascii_digit())
}

/// What a `%autopatch` line applies, `None` when it cannot be read (the line
/// then applies every patch, under itself as a guard).
fn autopatch_applies(words: &[&str]) -> Option<Applies> {
    let (mut lo, mut hi) = (None, None);
    let mut numbers = Vec::new();
    let mut i = 0;
    while i < words.len() {
        let word = words[i];
        i += 1;
        let option = ["-p", "-F", "-m", "-M"]
            .into_iter()
            .find(|o| word.starts_with(o));
        if let Some(option) = option {
            let value = match &word[2..] {
                "" => {
                    i += 1;
                    words.get(i - 1).copied().unwrap_or("")
                }
                glued => glued,
            };
            match option {
                "-m" | "-M" if !is_number(value) => return None,
                "-m" => lo = value.parse().ok(),
                "-M" => hi = value.parse().ok(),
                _ => {}
            }
        } else if word.starts_with('-') {
            continue;
        } else if is_number(word) {
            numbers.push(word.to_string());
        } else {
            return None;
        }
    }
    Some(if !numbers.is_empty() {
        Applies::Numbers(numbers)
    } else if lo.is_some() || hi.is_some() {
        Applies::Range(lo, hi)
    } else {
        Applies::All
    })
}

/// What a `%patch` line applies: the number glued to the macro (`%patch1`),
/// every `-PN` or `-P N`, and every positional number (`%patch 1`, rpm 4.18+);
/// the bare `%patch` is patch 0. `None` when a number cannot be read; `Some`
/// of nothing for `-R`, which unapplies.
fn patch_applies(word: &str, words: &[&str]) -> Option<Applies> {
    if words.iter().any(|w| *w == "-R" || *w == "--reverse") {
        return Some(Applies::Numbers(Vec::new()));
    }
    let mut numbers = Vec::new();
    let glued = &word["%patch".len()..];
    if !glued.is_empty() {
        if !is_number(glued) {
            return None;
        }
        numbers.push(glued.to_string());
    }
    let mut i = 0;
    while i < words.len() {
        let w = words[i];
        i += 1;
        let option = PATCH_VALUE_OPTIONS.iter().find(|o| w.starts_with(**o));
        if let Some(option) = option {
            let value = match &w[2..] {
                "" => {
                    i += 1;
                    words.get(i - 1).copied().unwrap_or("")
                }
                glued => glued,
            };
            if *option == "-P" {
                if !is_number(value) {
                    return None;
                }
                numbers.push(value.to_string());
            }
        } else if w.starts_with('-') {
            continue;
        } else if is_number(w) {
            numbers.push(w.to_string());
        } else {
            return None;
        }
    }
    if numbers.is_empty() && glued.is_empty() {
        numbers.push("0".to_string());
    }
    Some(Applies::Numbers(numbers))
}

/// The numbers a line names with `%{PATCHn}`, `%PATCHn` or `%{P:n}`.
fn macro_numbers(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    for prefix in ["%{PATCH", "%PATCH", "%{P:"] {
        for (at, _) in line.match_indices(prefix) {
            let digits: String = line[at + prefix.len()..]
                .chars()
                .take_while(char::is_ascii_digit)
                .collect();
            if !digits.is_empty() {
                out.push(digits);
            }
        }
    }
    out
}

/// What a line of `%prep` applies, with the guards it adds besides the frames.
fn application(line: &str) -> Option<(Applies, Option<String>)> {
    let words: Vec<&str> = line.split_whitespace().collect();
    let word = *words.first()?;
    let rest = &words[1..];
    let own_guard = || Some(collapse(line));
    match word {
        "%autosetup" | "%forgeautosetup" => {
            let no_patches = rest
                .iter()
                .any(|w| w.starts_with('-') && !w.starts_with("--") && w.contains('N'));
            (!no_patches).then_some((Applies::All, None))
        }
        "%autopatch" => Some(match autopatch_applies(rest) {
            Some(applies) => (applies, None),
            None => (Applies::All, own_guard()),
        }),
        w if w.starts_with("%patch") => match patch_applies(w, rest) {
            Some(applies) => Some((applies, None)),
            None if w["%patch".len()..].is_empty() || is_number(&w["%patch".len()..]) => {
                Some((Applies::All, own_guard()))
            }
            None => None,
        },
        _ => {
            let numbers = macro_numbers(line);
            (!numbers.is_empty()).then_some((Applies::Numbers(numbers), None))
        }
    }
}

fn scan(spec: &str) -> Scan {
    let macros = crate::spec::collect_simple_defines(spec);
    let mut stack = CondStack::new();
    let mut in_prep = false;
    let mut out = Scan {
        declarations: Vec::new(),
        applications: Vec::new(),
    };
    for line in spec.lines() {
        if stack.feed(line, &macros) {
            continue;
        }
        stack.declare(line);
        let skipped = stack.taken() == Some(false);
        if !skipped && let Some(section) = section_word(line) {
            in_prep = section == "prep";
        }
        let trimmed = line.trim();
        if trimmed.starts_with('#') {
            continue;
        }
        if let Some(patch) = crate::spec::parse_source_line(line, &["patch"], str::to_string) {
            out.declarations.push(Declaration {
                number: patch.index.to_string(),
                name: crate::pipeline::expand_patch_name(patch.url.trim(), spec),
                guards: stack.guards(),
                skipped,
            });
            continue;
        }
        if skipped || !in_prep {
            continue;
        }
        if let Some((applies, own)) = application(trimmed) {
            let mut guards = stack.guards();
            guards.extend(own);
            out.applications.push(Application { applies, guards });
        }
    }
    out
}

fn applies_to(applies: &Applies, number: &str) -> bool {
    match applies {
        Applies::All => true,
        Applies::Numbers(numbers) => numbers
            .iter()
            .any(|n| crate::pipeline::same_patch_number(n, number)),
        Applies::Range(lo, hi) => number
            .parse::<u64>()
            .is_ok_and(|n| lo.is_none_or(|lo| n >= lo) && hi.is_none_or(|hi| n <= hi)),
    }
}

/// `declaration` guards followed by `application` guards, duplicates dropped,
/// order kept.
fn join_guards(declaration: &[String], application: &[String]) -> GuardPath {
    let mut path: GuardPath = Vec::new();
    for guard in declaration.iter().chain(application) {
        if !path.contains(guard) {
            path.push(guard.clone());
        }
    }
    path
}

/// For each patch the spec declares (a declaration under an always false
/// frame is not one), the guard paths under which `%prep` applies it: the
/// declaration's guards followed by the application line's. No path: never
/// applied.
pub fn applied_paths(spec: &str) -> BTreeMap<String, BTreeSet<GuardPath>> {
    let scan = scan(spec);
    let mut out: BTreeMap<String, BTreeSet<GuardPath>> = BTreeMap::new();
    for declaration in scan.declarations.iter().filter(|d| !d.skipped) {
        let paths = out.entry(declaration.name.clone()).or_default();
        for app in &scan.applications {
            if applies_to(&app.applies, &declaration.number) {
                paths.insert(join_guards(&declaration.guards, &app.guards));
            }
        }
    }
    out
}

/// Patches `before` applies and `after` still declares (by
/// [`declared_patches`](super::declared_patches), which ignores `%if`), but no
/// longer applies everywhere it did. The patches `after` no longer declares
/// are removals, not these.
pub fn disabled_patches(before: &str, after: &str) -> Vec<(String, Disabled)> {
    let was = applied_paths(before);
    let now = applied_paths(after);
    let still_declared: BTreeSet<String> = super::declared_patches(after)
        .into_iter()
        .map(|(_, name)| name)
        .collect();
    let mut out = Vec::new();
    for (name, b) in &was {
        if b.is_empty() || !still_declared.contains(name) {
            continue;
        }
        let Some(a) = now.get(name) else {
            let guard = scan(after)
                .declarations
                .iter()
                .find(|d| d.skipped && d.name == *name)
                .map(|d| d.guards.join(", "))
                .unwrap_or_default();
            out.push((name.clone(), Disabled::NotDeclared { guard }));
            continue;
        };
        if a.contains(&Vec::new()) || b.is_subset(a) {
            continue;
        }
        if a.is_empty() {
            out.push((name.clone(), Disabled::NotApplied));
            continue;
        }
        let known: BTreeSet<&String> = b.iter().flatten().collect();
        let guard = a
            .iter()
            .flatten()
            .find(|g| !known.contains(g))
            .or_else(|| b.difference(a).flatten().next())
            .or_else(|| a.iter().flatten().next())
            .cloned()
            .unwrap_or_default();
        out.push((name.clone(), Disabled::Conditional { guard }));
    }
    out
}
