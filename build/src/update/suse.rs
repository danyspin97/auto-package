//! SUSE-maintained packages with no upstream release.
//!
//! Such a package is reported as "SUSE-maintained, no upstream release" when
//! nothing in its spec can give a newer version: its repository on a SUSE
//! forge (github.com/openSUSE, github.com/SUSE, code.opensuse.org,
//! src.opensuse.org) has no release and no tag, or its `Source:` has no URL
//! and no `_service` fetches it. The forge lookup runs first, so a tagged
//! repository is updated like any other.

use super::*;

/// The end of the forge answer that says a repository has neither a release
/// nor a tag (see `sources::latest_release`).
const NO_RELEASE: &str = "no release or tag in the answer";

/// Whether `url` is a repository of SUSE's on one of the forges above.
pub(super) fn is_suse_repo(url: &str) -> bool {
    let Some(forge) = ap_ecosystem::forge::parse(url) else {
        return false;
    };
    let host = forge.host.to_ascii_lowercase();
    let owner = forge
        .project_path
        .split('/')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    match host.as_str() {
        "github.com" => owner == "opensuse" || owner == "suse",
        "code.opensuse.org" | "src.opensuse.org" => true,
        _ => false,
    }
}

/// Why `header`'s package is SUSE-maintained with no upstream release, when it
/// is. `forge_failure` is the first failed lookup of the package; `has_service`
/// says whether the package holds a `_service`, asked only when it is needed.
pub(super) fn no_upstream_release(
    header: &spec::Header,
    forge_failure: Option<&SkipReason>,
    has_service: impl FnOnce() -> bool,
) -> Option<String> {
    if let Some(SkipReason::LookupFailed {
        ecosystem_tag,
        name,
        error,
    }) = forge_failure
        && ecosystem_tag == "git"
        && is_suse_repo(name)
        && error.contains(NO_RELEASE)
    {
        return Some(format!("{name} has no release or tag"));
    }
    let source = header.source.as_deref()?;
    let expanded = header.expanded_source.as_deref().unwrap_or(source);
    if has_url(source) || has_url(expanded) || has_service() {
        return None;
    }
    Some("the Source: has no URL and no _service fetches it".to_string())
}

fn has_url(source: &str) -> bool {
    source.contains("://") || source.starts_with("git@")
}

/// Whether a `_service` is among the files of `pkg` in `project`. When OBS
/// cannot say, the answer is yes: the package is then not reported as having
/// no upstream.
pub fn project_has_service(project: &str, pkg: &str, emit: Emit<'_>) -> bool {
    osc::list_package_files(project, pkg, emit)
        .is_none_or(|files| files.iter().any(|f| f == "_service"))
}
