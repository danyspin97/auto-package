//! The `lint` of a package whose source auto-package does not recognise: a
//! repository release with no build system it knows. The build pipeline would
//! refuse such a package (it has no recipe of its own to keep for a package
//! with no notes), so `lint` does not send it there: its branch leftovers are
//! cleaned as for a tool-only package, and the source is the reason it is
//! skipped.

use super::*;

/// Why a package of an unrecognised source is skipped by `lint`.
pub(super) const SKIPPED: &str = "its source is not one auto-package recognises: its branch leftovers were cleaned, and it was not built";

/// Whether `item`'s source is one auto-package does not recognise. `probe`
/// tells, for (repository url, version, package name), whether the release
/// archive has no build system auto-package knows (see [`probe_archive`]). Only
/// a repository package is probed; a probe that fails is no answer, so the
/// package takes the pipeline as before, which reports the same failure.
pub(super) fn unrecognised(
    item: &Item,
    probe: &dyn Fn(&str, &str, &str) -> Result<bool, String>,
) -> bool {
    let Ecosystem::GitRepo { url } = &item.ecosystem else {
        return false;
    };
    matches!(
        probe(url, &item.new_version, &repo_package_name(url)),
        Ok(true)
    )
}

/// Whether the release archive of a repository package has no build system
/// auto-package knows: the archive is downloaded and read, as the pipeline does.
pub(super) fn probe_archive(url: &str, version: &str, name: &str) -> Result<bool, String> {
    let probe = ap_build::sources::probe(url, version, name, None)?;
    Ok(matches!(
        probe.ecosystem,
        ap_build::sources::Ecosystem::Unknown
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    fn item(ecosystem: Ecosystem) -> Item {
        Item {
            pkg: "pyenv".to_string(),
            origin: String::new(),
            ecosystem,
            old_version: "2.8.15".to_string(),
            new_version: "2.8.15".to_string(),
            upstream_tag: None,
            held_version: None,
            dependency_of: None,
            requires: Vec::new(),
            findings: Vec::new(),
        }
    }

    fn repo() -> Ecosystem {
        Ecosystem::GitRepo {
            url: "https://github.com/pyenv/pyenv".to_string(),
        }
    }

    #[test]
    fn a_repository_whose_archive_has_no_known_build_system_is_unrecognised() {
        let asked = RefCell::new(Vec::new());
        let probe = |url: &str, version: &str, name: &str| {
            asked
                .borrow_mut()
                .push((url.to_string(), version.to_string(), name.to_string()));
            Ok(true)
        };
        assert!(unrecognised(&item(repo()), &probe));
        assert_eq!(
            asked.borrow().as_slice(),
            [(
                "https://github.com/pyenv/pyenv".to_string(),
                "2.8.15".to_string(),
                "pyenv".to_string()
            )]
        );
    }

    #[test]
    fn a_recognised_archive_is_not_unrecognised() {
        let python = |_: &str, _: &str, _: &str| Ok(false);
        let plain = |_: &str, _: &str, _: &str| Ok(false);
        assert!(!unrecognised(&item(repo()), &python));
        assert!(!unrecognised(&item(repo()), &plain));
    }

    #[test]
    fn a_failed_probe_takes_the_pipeline_as_before() {
        let failed =
            |_: &str, _: &str, _: &str| Err("the archive could not be fetched".to_string());
        assert!(!unrecognised(&item(repo()), &failed));
    }

    #[test]
    fn only_a_repository_package_is_probed() {
        let never = |_: &str, _: &str, _: &str| -> Result<bool, String> {
            panic!("a PyPI package is not probed")
        };
        let pypi = Ecosystem::Pypi {
            pypi_name: "pyenv".to_string(),
        };
        assert!(!unrecognised(&item(pypi), &never));
    }
}
