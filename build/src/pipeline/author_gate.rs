//! The author of the `.changes` entries (see `osc::changes_author`) as the
//! pipeline asks for it.

use super::*;

/// The author of this machine. The pipeline's tests run on machines of any
/// identity, so under test it is fixed; `osc::resolve_author` has its own
/// tests with injected environments.
pub(super) fn host_author(checkout: Option<&Path>) -> Result<osc::Author, String> {
    #[cfg(test)]
    {
        let _ = checkout;
        Ok(osc::Author {
            name: "Test Author".into(),
            email: "test@example.org".into(),
        })
    }
    #[cfg(not(test))]
    osc::host_author(checkout)
}

/// The author for the `_service` of a run: the resolved one, or the reason
/// there is none. A local build commits nothing, so it needs none: without
/// one its service writes no entry (`None`) rather than one by a stand-in.
pub(super) fn service_author(
    local: bool,
    resolved: Result<osc::Author, String>,
) -> Result<Option<osc::Author>, String> {
    match resolved {
        Ok(author) => Ok(Some(author)),
        Err(_) if local => Ok(None),
        Err(reason) => Err(reason),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn who() -> osc::Author {
        osc::Author {
            name: "A".into(),
            email: "a@example.org".into(),
        }
    }

    #[test]
    fn a_run_that_commits_needs_the_author_in_its_service() {
        assert_eq!(service_author(false, Ok(who())).unwrap(), Some(who()));
        assert_eq!(
            service_author(false, Err("no author".into())).unwrap_err(),
            "no author"
        );
    }

    #[test]
    fn a_local_build_without_an_author_writes_no_entry() {
        assert_eq!(service_author(true, Err("no author".into())).unwrap(), None);
        assert_eq!(service_author(true, Ok(who())).unwrap(), Some(who()));
    }
}
