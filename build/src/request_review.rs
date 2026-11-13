//! A read-only review of an OBS submit request: the request is parsed, the
//! source and the target are read, and deterministic checks judge what the
//! request changes. Every finding has a stable id, a severity and an origin;
//! the checks never write to OBS.

use serde::Serialize;

pub mod agent_turn;
#[cfg(test)]
mod agent_turn_tests;
pub mod archive;
pub mod build_check;
#[cfg(test)]
mod build_check_tests;
pub mod changes_checks;
#[cfg(test)]
mod changes_checks_tests;
pub mod downloads;
#[cfg(test)]
mod downloads_tests;
pub mod fetch;
#[cfg(test)]
mod fetch_fake;
pub mod gather;
#[cfg(test)]
mod gather_tests;
pub mod ids;
pub mod patch_checks;
#[cfg(test)]
mod patch_checks_tests;
pub mod provenance_check;
#[cfg(test)]
mod read_only_tests;
pub mod render;
pub mod report;
#[cfg(test)]
mod report_tests;
pub mod request;
pub mod risk;
pub mod run;
#[cfg(test)]
mod run_tests;
pub mod save;
pub mod signature_checks;
pub mod source_checks;
#[cfg(test)]
mod source_checks_tests;
pub mod spec_checks;
pub mod synthetic;
#[cfg(test)]
mod synthetic_tests;
pub mod upstream_check;
#[cfg(test)]
mod upstream_check_tests;

/// How much a finding matters. `Note < Warning < Blocker`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Note,
    Warning,
    Blocker,
}

impl Severity {
    pub fn word(self) -> &'static str {
        match self {
            Severity::Note => "note",
            Severity::Warning => "warning",
            Severity::Blocker => "blocker",
        }
    }
}

/// Who found it: a deterministic check, or the model (an opinion).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Origin {
    Check,
    Agent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Finding {
    pub id: &'static str,
    pub severity: Severity,
    pub origin: Origin,
    /// The target package of the action, empty for the request as a whole.
    pub package: String,
    pub file: Option<String>,
    pub line: Option<usize>,
    /// One line.
    pub message: String,
}

/// `text` as one plain line: every white space is one space, and every
/// invisible, control or direction-changing character is dropped, so a name or
/// a line from a package cannot move the cursor, clear the screen or reorder
/// what the reader sees. Cut like `fix_flags::one_line`.
pub fn plain_line(text: &str) -> String {
    let shown: String = text
        .chars()
        .map(|c| if c.is_whitespace() { ' ' } else { c })
        .filter(|c| !crate::pipeline::is_hidden(*c))
        .collect();
    crate::fix_flags::one_line(&shown)
}

impl Finding {
    /// A finding of a deterministic check. Every text of it is [`plain_line`].
    pub fn check(id: &'static str, severity: Severity, package: &str, message: &str) -> Self {
        Finding {
            id,
            severity,
            origin: Origin::Check,
            package: plain_line(package),
            file: None,
            line: None,
            message: plain_line(message),
        }
    }

    pub fn in_file(mut self, file: &str, line: Option<usize>) -> Self {
        self.file = Some(plain_line(file));
        self.line = line;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn severities_order_from_note_to_blocker() {
        assert!(Severity::Note < Severity::Warning);
        assert!(Severity::Warning < Severity::Blocker);
    }

    #[test]
    fn a_terminal_escape_in_a_finding_text_is_dropped() {
        let attack = "bullet \x1b[2A\x1b[J\u{202e}reversed\u{200b}\x07 end";
        let f = Finding::check(ids::REQUEST_COMPETING, Severity::Note, "p\x1b[2J", attack)
            .in_file("a\x1b[Jb.spec", None);
        for text in [
            f.message.as_str(),
            f.package.as_str(),
            f.file.as_deref().unwrap(),
        ] {
            assert!(
                !text
                    .chars()
                    .any(|c| c.is_control() || c == '\u{202e}' || c == '\u{200b}'),
                "{text:?}"
            );
        }
        assert_eq!(f.message, "bullet [2A[Jreversed end");
        assert_eq!(f.package, "p[2J");
    }

    #[test]
    fn a_finding_message_is_one_line() {
        let f = Finding::check(ids::REQUEST_COMPETING, Severity::Warning, "p", "a\n  b\tc");
        assert_eq!(f.message, "a b c");
        assert_eq!(f.origin, Origin::Check);
        let f = f.in_file("p.spec", Some(3));
        assert_eq!((f.file.as_deref(), f.line), (Some("p.spec"), Some(3)));
    }
}
