//! The error of a run that could not check every package it was asked about.

use super::*;

/// The error of a run whose specs could not be read from OBS, or whose upstream
/// lookups failed for a reason that may pass: `unreadable` holds each package
/// with the reason.
pub(super) fn obs_unavailable(unreadable: &[String], source: &str) -> color_eyre::Report {
    eyre!(
        "the run is incomplete: {} package(s) in {source} could not be read or looked up, so \
         they were not checked: {}",
        unreadable.len(),
        unreadable.join("; ")
    )
}

#[cfg(test)]
mod tests {
    use super::obs_unavailable;

    #[test]
    fn the_unreadable_packages_are_named_under_the_obs_message() {
        let err = obs_unavailable(&["foo: listing failed".to_string()], "Base:System");
        let text = err.to_string();
        assert!(
            text.starts_with("the run is incomplete: 1 package(s) in Base:System"),
            "{text}"
        );
        assert!(text.ends_with("foo: listing failed"), "{text}");
    }
}
