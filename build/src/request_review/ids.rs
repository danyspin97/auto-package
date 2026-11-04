//! The ids of the review's findings. They are part of the JSON contract: a
//! renamed id breaks the test below, which lists every one.

macro_rules! ids {
    ($($name:ident = $id:literal,)*) => {
        $(pub const $name: &str = $id;)*
        /// Every id, in the order they are declared.
        pub const ALL: &[&str] = &[$($id),*];
    };
}

ids! {
    REQUEST_FINAL_STATE = "request.final-state",
    REQUEST_ACTION_NOT_REVIEWED = "request.action-not-reviewed",
    REQUEST_NO_SOURCE_REV = "request.no-source-rev",
    REQUEST_ACTION_MALFORMED = "request.action-malformed",
    REQUEST_COMPETING = "request.competing",
    REVIEW_INCOMPLETE = "review.incomplete",
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_id_is_listed_once_with_its_stable_spelling() {
        assert_eq!(
            ALL,
            [
                "request.final-state",
                "request.action-not-reviewed",
                "request.no-source-rev",
                "request.action-malformed",
                "request.competing",
                "review.incomplete",
            ]
        );
        let mut sorted = ALL.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), ALL.len());
    }
}
