//! The skip for a package that an open submit request already updates.

use std::fmt;

/// An open submit request to the package's origin that brings a version at
/// least as new as the planned target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlreadyRequested {
    pub id: String,
    pub package: String,
    /// The version the request brings.
    pub version: String,
    pub creator: String,
    /// `new` or `review`.
    pub state: String,
}

/// A version as it is shown and kept: without control and bidi characters
/// (`pipeline::is_hidden`) and surrounding space, at most 64 characters. `None`
/// when nothing is left.
pub fn clean_version(raw: &str) -> Option<String> {
    let kept: String = raw
        .chars()
        .filter(|c| !crate::pipeline::is_hidden(*c))
        .take(64)
        .collect();
    let kept = kept.trim();
    (!kept.is_empty()).then(|| kept.to_string())
}

impl fmt::Display for AlreadyRequested {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "request {} by {} brings {} to {}: not duplicating the work",
            self.id, self.creator, self.version, self.package
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_version_is_cleaned_for_display_and_keeping() {
        assert_eq!(clean_version("1.\x1b[2J0").as_deref(), Some("1.[2J0"));
        assert_eq!(clean_version(" 1.2\u{202e}3 ").as_deref(), Some("1.23"));
        assert_eq!(clean_version("\x1b\x07"), None);
        let long = "9".repeat(100);
        assert_eq!(clean_version(&long).unwrap().len(), 64);
    }

    #[test]
    fn the_skip_names_the_request_and_its_creator() {
        let why = super::super::SkipReason::AlreadyRequested(AlreadyRequested {
            id: "42".into(),
            package: "foo".into(),
            version: "1.2".into(),
            creator: "alice".into(),
            state: "review".into(),
        });
        assert_eq!(
            why.to_string(),
            "request 42 by alice brings 1.2 to foo: not duplicating the work"
        );
    }
}
