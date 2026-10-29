//! The npm vendor route: the lockfile checks, the node modules service and the
//! vendor files it stages.

use super::*;

/// Enforce the minimum shape of a `UseUpstreamWheel`'s `why`: non-empty, long
/// enough to carry an actual quoted build-log line or missing-toolchain
/// name, not a bare "idk" or whitespace.
///
/// Not evidence verification — the agent could still fabricate a plausible
/// string — but a cheap gate against the obvious trivial-reason case, and
/// paired with the mandatory provenance file ([`write_wheel_switch_provenance`])
/// it makes the switch auditable rather than a silent recipe swap. Returns
/// `Err` for the caller to fold into its own failure path.
///
/// The 20-char floor is the length of the shortest realistic
/// evidence line the Exception 4 wording asks for (`"nvcc: not found"` is 15,
/// `"find_package(CUDAToolkit"` is 23); it rejects "" / "no reason" / the
/// placeholder shapes without rejecting any genuine single-line citation.
/// Marker written to the notes when the wheel switch was deferred in favour
/// of npm vendoring, so the second request goes through.
pub(super) const NPM_VENDOR_NOTE_MARKER: &str = "[npm-vendor-route-suggested]";

/// Whether a `.tar.gz` sdist ships a `Cargo.toml` at its top level.
pub(super) fn tarball_has_root_cargo_toml(tarball: &Path) -> bool {
    let Ok(file) = std::fs::File::open(tarball) else {
        return false;
    };
    let mut ar = ap_ecosystem::sdist::bounded_tar_gz(file);
    let Ok(entries) = ar.entries() else {
        return false;
    };
    entries
        .take(ap_ecosystem::sdist::SCAN_MAX_ENTRIES)
        .flatten()
        .any(|e| {
            e.path()
                .is_ok_and(|p| p.components().count() == 2 && p.ends_with("Cargo.toml"))
        })
}

/// Whether a `.tar.gz` sdist ships a `Cargo.lock` at its top level, or next to
/// one of `manifests` (paths relative to the archive root).
pub(super) fn tarball_has_cargo_lock(tarball: &Path, manifests: &[String]) -> bool {
    let Ok(file) = std::fs::File::open(tarball) else {
        return false;
    };
    let mut ar = ap_ecosystem::sdist::bounded_tar_gz(file);
    let Ok(entries) = ar.entries() else {
        return false;
    };
    entries
        .take(ap_ecosystem::sdist::SCAN_MAX_ENTRIES)
        .flatten()
        .any(|e| {
            e.path().is_ok_and(|p| {
                if !p.ends_with("Cargo.lock") {
                    return false;
                }
                let rel: PathBuf = p.components().skip(1).collect();
                rel.parent().is_some_and(|dir| {
                    dir.as_os_str().is_empty()
                        || manifests.iter().any(|m| Path::new(m).parent() == Some(dir))
                })
            })
        })
}

/// Whether a `.tar.gz` sdist ships a `package-lock.json` (the input the
/// `node_modules` service reads; it cannot use pnpm-lock.yaml or yarn.lock).
fn tarball_has_npm_lockfile(tarball: &Path) -> bool {
    let Ok(file) = std::fs::File::open(tarball) else {
        return false;
    };
    let mut ar = ap_ecosystem::sdist::bounded_tar_gz(file);
    let Ok(entries) = ar.entries() else {
        return false;
    };
    entries
        .take(ap_ecosystem::sdist::SCAN_MAX_ENTRIES)
        .flatten()
        .any(|e| {
            e.path().is_ok_and(|p| {
                p.file_name().is_some_and(|n| n == "package-lock.json")
                    && !p.components().any(|c| c.as_os_str() == "node_modules")
            })
        })
}

/// The contents of the shallowest `package-lock.json` of a `.tar.gz` sdist that
/// is not inside a `node_modules` directory.
fn tarball_npm_lockfile(tarball: &Path) -> Option<Vec<u8>> {
    use std::io::Read;
    let file = std::fs::File::open(tarball).ok()?;
    let mut ar = ap_ecosystem::sdist::bounded_tar_gz(file);
    let mut best: Option<(usize, Vec<u8>)> = None;
    for mut entry in ar
        .entries()
        .ok()?
        .take(ap_ecosystem::sdist::SCAN_MAX_ENTRIES)
        .flatten()
    {
        let Ok(path) = entry.path().map(|p| p.into_owned()) else {
            continue;
        };
        if path.file_name().is_none_or(|n| n != "package-lock.json")
            || path.components().any(|c| c.as_os_str() == "node_modules")
            || !entry.header().entry_type().is_file()
        {
            continue;
        }
        let depth = path.components().count();
        if best.as_ref().is_some_and(|(d, _)| *d <= depth) {
            continue;
        }
        let mut bytes = Vec::new();
        if entry
            .by_ref()
            .take(crate::stage::MAX_STAGE_BYTES + 1)
            .read_to_end(&mut bytes)
            .is_ok()
            && bytes.len() as u64 <= crate::stage::MAX_STAGE_BYTES
        {
            best = Some((depth, bytes));
        }
    }
    best.map(|(_, b)| b)
}

/// The `node_modules` service entry of the npm vendoring route.
const NODE_MODULES_SERVICE_ENTRY: &str = "  <service name=\"node_modules\" mode=\"manual\">\n\
     \x20   <param name=\"input\">package-lock.json</param>\n\
     \x20 </service>\n";

/// `_service` text with the `node_modules` service added; `None` when it
/// already has one or is not a `<services>` document.
fn with_node_modules_service(existing: Option<&str>) -> Option<String> {
    match existing {
        None => Some(format!(
            "<services>\n{NODE_MODULES_SERVICE_ENTRY}</services>\n"
        )),
        Some(xml) if xml.contains("name=\"node_modules\"") => None,
        Some(xml) => {
            let at = xml.rfind("</services>")?;
            Some(format!(
                "{}{NODE_MODULES_SERVICE_ENTRY}{}",
                &xml[..at],
                &xml[at..]
            ))
        }
    }
}

/// The checkout files the npm vendoring route writes (see
/// [`stage_npm_vendor_route`]).
pub(super) const NPM_VENDOR_FILES: [&str; 2] = ["package-lock.json", "_service"];

/// The file work of the npm vendoring route, done by the pipeline: copy the
/// sdist's `package-lock.json` into the checkout and add the `node_modules`
/// service to `_service`. Returns what was done, for the notes.
pub(super) fn stage_npm_vendor_route(
    checkout_dir: &Path,
    version: &str,
) -> Result<Vec<String>, String> {
    let tarball = find_tarball(checkout_dir, version)
        .ok_or_else(|| "the checkout has no source archive".to_string())?;
    let lock = tarball_npm_lockfile(&tarball)
        .ok_or_else(|| format!("{} has no usable package-lock.json", tarball.display()))?;
    let mut done = Vec::new();
    let dest = checkout_dir.join("package-lock.json");
    if std::fs::read(&dest).ok().as_deref() != Some(lock.as_slice()) {
        ap_core::fsx::write_atomic(&dest, &lock)
            .map_err(|e| format!("writing package-lock.json: {e}"))?;
    }
    done.push("copied package-lock.json from the sdist into the checkout".to_string());
    let service = checkout_dir.join("_service");
    let existing = std::fs::read_to_string(&service).ok();
    match with_node_modules_service(existing.as_deref()) {
        Some(updated) => {
            ap_core::fsx::write_atomic(&service, updated)
                .map_err(|e| format!("writing _service: {e}"))?;
            done.push("added the `node_modules` service to `_service`".to_string());
        }
        None if existing
            .as_deref()
            .is_some_and(|x| x.contains("name=\"node_modules\"")) =>
        {
            done.push("found the `node_modules` service already in `_service`".to_string());
        }
        None => return Err("the existing _service is not a <services> document".to_string()),
    }
    Ok(done)
}

/// Whether the notes record an npm vendoring deferral (the marker
/// `npm_vendor_route_pending` reads).
pub(super) fn notes_record_npm_deferral(notes_path: &Path) -> bool {
    std::fs::read_to_string(notes_path).is_ok_and(|n| n.contains(NPM_VENDOR_NOTE_MARKER))
}

/// True when a `use_upstream_wheel` should first be turned into an npm
/// vendoring attempt: the sdist has a package-lock.json outside `node_modules`
/// and that redirect has not been given yet. Read from the sdist and the
/// notes, not from the agent's wording, so the route is taken once per package.
pub(super) fn npm_vendor_route_pending(
    checkout_dir: &Path,
    notes_path: &Path,
    version: &str,
) -> bool {
    if std::fs::read_to_string(notes_path).is_ok_and(|n| n.contains(NPM_VENDOR_NOTE_MARKER)) {
        return false;
    }
    find_tarball(checkout_dir, version).is_some_and(|t| tarball_has_npm_lockfile(&t))
}
