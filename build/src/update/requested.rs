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
