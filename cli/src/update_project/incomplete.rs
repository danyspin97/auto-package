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

/// What a run that checked no package ends with: an error when `--only` named
/// packages (they were asked for and none could be checked), else nothing.
pub(super) fn none_checked(only: &[String], absent: bool) -> color_eyre::Result<()> {
    if only.is_empty() {
        return Ok(());
    }
    let why = if absent { "not on OBS: " } else { "" };
    Err(eyre!(
        "none of the requested packages could be checked: {why}{}",
        only.join(", ")
    ))
}

#[cfg(test)]
mod none_checked_tests {
    use super::none_checked;

    #[test]
    fn a_run_that_checked_nothing_fails_only_when_packages_were_asked_for() {
        assert!(none_checked(&[], false).is_ok());
        let err = none_checked(&["foo".to_string(), "bar".to_string()], false).unwrap_err();
        assert!(
            err.to_string()
                .starts_with("none of the requested packages could be checked"),
            "{err}"
        );
        let absent = none_checked(&["gone".to_string()], true)
            .unwrap_err()
            .to_string();
        assert_eq!(
            absent,
            "none of the requested packages could be checked: not on OBS: gone"
        );
    }
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
