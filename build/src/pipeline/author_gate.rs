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

/// What the preflight does with the resolved author: the package stops with
/// the one-line reason, as an environment failure (a rerun repeats it until
/// the identity is set, and no agent turn can fix it).
pub(super) fn author_or_stop(
    rpm_name: &str,
    resolved: Result<osc::Author, String>,
) -> Result<osc::Author, PipelineResult> {
    resolved.map_err(|reason| PipelineResult::Failed {
        rpm_name: rpm_name.to_string(),
        reason,
        kind: FailureKind::Environment,
        remedy: None,
        note: None,
    })
}

/// The preflight check: a run that commits stops before any work when no
/// author resolves. Nothing is asked of the identity under `--local`, which
/// commits nothing. `checkout` is the package's checkout, so the author is
/// resolved for the API the later gates use (`.osc/_apiurl`). A package with
/// no checkout yet resolves for osc's default API, which is also the API its
/// `osc co` will use (no run names another), so the gates agree.
pub(super) fn require_author(
    rpm_name: &str,
    local: bool,
    checkout: &Path,
    resolve: &dyn Fn(Option<&Path>) -> Result<osc::Author, String>,
) -> Result<(), PipelineResult> {
    if local {
        return Ok(());
    }
    author_or_stop(rpm_name, resolve(Some(checkout))).map(|_| ())
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

    #[test]
    fn an_unresolved_author_stops_the_package_as_an_environment_failure() {
        match author_or_stop("p", Err("no changelog author".into())) {
            Err(PipelineResult::Failed {
                rpm_name,
                reason,
                kind,
                remedy,
                ..
            }) => {
                assert_eq!(rpm_name, "p");
                assert_eq!(reason, "no changelog author");
                assert_eq!(kind, FailureKind::Environment);
                assert!(remedy.is_none());
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(author_or_stop("p", Ok(who())).unwrap(), who());
    }

    #[test]
    fn the_preflight_resolves_for_the_checkout_and_local_asks_nothing() {
        let seen = std::cell::RefCell::new(None);
        let resolve = |c: Option<&Path>| {
            *seen.borrow_mut() = c.map(Path::to_path_buf);
            Err("none".to_string())
        };
        let checkout = Path::new("/w/proj/p");
        assert!(require_author("p", false, checkout, &resolve).is_err());
        assert_eq!(seen.borrow().as_deref(), Some(checkout));
        *seen.borrow_mut() = None;
        assert!(require_author("p", true, checkout, &resolve).is_ok());
        assert_eq!(*seen.borrow(), None);
    }
}
