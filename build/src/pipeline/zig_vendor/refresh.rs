//! Refreshes a package's vendor archive from the new Source0: reads the
//! manifest of its zig package, fetches and checks each dependency, and writes
//! the archive over the old one under the same name.

use std::io::{BufReader, BufWriter, Read, Write};
use std::path::Path;

use ap_core::sink::Reporter;

use super::fetch::{Fetch, HttpFetch, RETRY_PAUSE, Refusal, Request, fetch_vendor};
use super::plan::{plan, source0_file};
use super::unpack::{Format, LIMITS, unpack};
use super::vendor_tar::write_vendor_tar;
use super::zon;

/// What a refresh did.
#[derive(Debug)]
pub(in crate::pipeline) struct Refreshed {
    /// The vendor archive's file name in the checkout.
    pub(in crate::pipeline) file: String,
    /// Whether the file's bytes changed. A refresh that writes what the old
    /// archive already had is not a change to the package.
    pub(in crate::pipeline) changed: bool,
    /// The dependencies the archive holds, sorted by declared hash.
    pub(in crate::pipeline) deps: Vec<RefreshedDep>,
}

/// A dependency the archive holds, as the manifest of its package gives it.
#[derive(Debug, Clone)]
pub(in crate::pipeline) struct RefreshedDep {
    pub(in crate::pipeline) name: String,
    pub(in crate::pipeline) url: String,
    pub(in crate::pipeline) hash: String,
}

/// [`refresh_checkout`] over the network, through the host allowlist. For the
/// update pipeline.
pub(in crate::pipeline) fn refresh_from_network(
    checkout: &Path,
    spec: &str,
    package: &str,
    reporter: &dyn Reporter,
    work: &Path,
) -> Result<Option<Refreshed>, Refusal> {
    refresh_checkout(checkout, spec, package, &HttpFetch, reporter, work)
}

/// Whether [`refresh_from_network`] applies to `spec`. `Err` when the spec is
/// one it applies to but cannot refresh: the reason the package stops with.
pub(in crate::pipeline) fn applies(spec: &str) -> Result<bool, String> {
    plan(spec).map(|p| p.is_some())
}

/// Refresh the vendor archive of the spec in `checkout`. `Ok(None)` when the
/// spec is not one to refresh (see [`plan`]). `work` is a fresh directory the
/// archives unpack into. Nothing is written when any check fails.
pub(super) fn refresh_checkout(
    checkout: &Path,
    spec: &str,
    package: &str,
    fetch: &dyn Fetch,
    reporter: &dyn Reporter,
    work: &Path,
) -> Result<Option<Refreshed>, Refusal> {
    let Some(plan) = plan(spec)? else {
        return Ok(None);
    };
    let source0 =
        source0_file(spec).ok_or_else(|| "the spec's Source0 has no file name".to_string())?;
    let archive = checkout.join(&source0);
    let file = std::fs::File::open(&archive)
        .map_err(|e| format!("the new Source0 {source0} is not in the checkout: {e}"))?;
    let src_dir = work.join("source");
    let format = Format::of_extension(&source0).ok_or_else(|| {
        format!("the new Source0 {source0} has no archive extension zig_vendor reads")
    })?;
    unpack(file, format, &src_dir, LIMITS)
        .map_err(|e| format!("unpacking the new Source0 {source0}: {e}"))?;
    let text = std::fs::read_to_string(src_dir.join("build.zig.zon"))
        .map_err(|_| format!("the new Source0 {source0} has no build.zig.zon"))?;
    let manifest = zon::parse(&text).map_err(|e| format!("build.zig.zon: {e}"))?;
    let deps_dir = work.join("deps");
    std::fs::create_dir_all(&deps_dir)
        .map_err(|e| format!("creating {}: {e}", deps_dir.display()))?;
    let req = Request {
        package,
        fetch,
        work: &deps_dir,
        limits: LIMITS,
        reporter,
        pause: RETRY_PAUSE,
    };
    let deps = fetch_vendor(&req, &manifest)?;
    // The archive is streamed into a temp file next to its target, so a
    // failure part way leaves the old archive in place.
    let target = checkout.join(&plan.file);
    let mut tmp = tempfile::NamedTempFile::new_in(checkout)
        .map_err(|e| format!("creating a temp file in {}: {e}", checkout.display()))?;
    {
        let mut out = BufWriter::new(tmp.as_file_mut());
        write_vendor_tar(
            &deps,
            plan.layout,
            &plan.root,
            plan.compression,
            work,
            &mut out,
        )?;
        out.flush()
            .map_err(|e| format!("writing {}: {e}", plan.file))?;
    }
    let changed = differs(&target, tmp.path())
        .map_err(|e| format!("comparing {} with the new archive: {e}", plan.file))?;
    if changed {
        let bytes =
            std::fs::read(tmp.path()).map_err(|e| format!("reading the new archive: {e}"))?;
        ap_core::fsx::write_atomic(&target, &bytes)
            .map_err(|e| format!("writing {}: {e}", plan.file))?;
    }
    let deps = deps
        .into_iter()
        .map(|d| RefreshedDep {
            name: d.name,
            url: d.url,
            hash: d.declared,
        })
        .collect();
    Ok(Some(Refreshed {
        file: plan.file,
        changed,
        deps,
    }))
}

/// Whether `old` and `new` differ in their bytes. A missing `old` differs.
fn differs(old: &Path, new: &Path) -> std::io::Result<bool> {
    let Ok(old) = std::fs::File::open(old) else {
        return Ok(true);
    };
    let new = std::fs::File::open(new)?;
    if old.metadata()?.len() != new.metadata()?.len() {
        return Ok(true);
    }
    let mut a = BufReader::new(old);
    let mut b = BufReader::new(new);
    let mut buf_a = vec![0u8; 64 * 1024];
    let mut buf_b = vec![0u8; 64 * 1024];
    loop {
        let n = a.read(&mut buf_a)?;
        if n == 0 {
            return Ok(false);
        }
        b.read_exact(&mut buf_b[..n])?;
        if buf_a[..n] != buf_b[..n] {
            return Ok(true);
        }
    }
}
