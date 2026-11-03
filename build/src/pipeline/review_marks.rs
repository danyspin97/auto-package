//! What the workspace remembers about patch reviews: the patches a reviewer
//! rewrote itself (for the risk flags), the rewritten patches a stop left
//! unreviewed (a rerun refuses them), and the patches as they were when an agent
//! turn began (a crash cannot leave an unmarked rewrite). Records are bound to the
//! full SHA-256 of the patch bytes, and a record that cannot be written is an error.

use super::*;
use crate::pipeline::patch_rewrite::rewritten_patches;
use sha2::{Digest, Sha256};

const REWROTE: &str = "patch-review/rewritten";
const PENDING: &str = "patch-review/pending";
const IN_PROGRESS: &str = "patch-review/in-progress";
const START_DIR: &str = "patch-review/start";

fn hash_of(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A second place for a record the workspace cannot take: the directory that
/// holds every workspace, which this run can write when it can write one.
fn fallback(ws: &Workspace, file: &str) -> PathBuf {
    let dir = ws.dir();
    let base = dir.parent().unwrap_or(&dir);
    let name = dir.file_name().unwrap_or_default().to_string_lossy();
    base.join(format!("{name}-{}", file.replace('/', "-")))
}

fn append_to(path: &Path, line: &str) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut f = options.open(path)?;
    writeln!(f, "{line}")?;
    f.sync_all()
}

/// Writes `contents` to `path`, replacing it: atomic and private.
fn write_private(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    ap_core::fsx::write_atomic(path, contents)?;
    #[cfg(unix)]
    std::fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
    Ok(())
}

/// Appends a record of `name` at its current bytes, in the workspace or else in
/// its fallback place; `Err` when neither takes it.
fn append(ws: &Workspace, file: &str, checkout: &Path, name: &str) -> Result<(), String> {
    let bytes = std::fs::read(checkout.join(name)).unwrap_or_default();
    let line = format!("{name}\t{}", hash_of(&bytes));
    append_to(&ws.dir().join(file), &line).or_else(|first| {
        append_to(&fallback(ws, file), &line)
            .map_err(|second| format!("cannot record {name} ({first}; {second})"))
    })
}

/// The names the records of `file` hold (in either place) whose patch in
/// `checkout` still has the recorded bytes; `any` takes every name instead.
fn recorded(ws: &Workspace, file: &str, checkout: &Path, any: bool) -> Vec<String> {
    let mut last: std::collections::BTreeMap<String, String> = Default::default();
    for path in [ws.dir().join(file), fallback(ws, file)] {
        for line in std::fs::read_to_string(path).unwrap_or_default().lines() {
            if let Some((name, hash)) = line.split_once('\t') {
                last.insert(name.to_string(), hash.to_string());
            }
        }
    }
    last.into_iter()
        .filter(|(name, hash)| {
            any || std::fs::read(checkout.join(name)).is_ok_and(|b| hash_of(&b) == *hash)
        })
        .map(|(name, _)| name)
        .collect()
}

fn remove(ws: &Workspace, file: &str) {
    for path in [ws.dir().join(file), fallback(ws, file)] {
        if let Err(e) = std::fs::remove_file(&path)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            ap_core::diag!("  cannot remove {}: {e}", path.display());
        }
    }
}

/// Marks a patch a reviewer rewrote itself, at its current bytes.
pub(super) fn mark_reviewer_rewrite(
    ws: &Workspace,
    checkout: &Path,
    name: &str,
) -> Result<(), String> {
    append(ws, REWROTE, checkout, name)
}

/// The patches a reviewer rewrote that still have the bytes it left.
pub(super) fn reviewer_rewrites(ws: &Workspace, checkout: &Path) -> Vec<String> {
    recorded(ws, REWROTE, checkout, false)
}

/// Whether a reviewer ever rewrote `name` (whatever its bytes are now).
pub(super) fn was_reviewer_rewrite(ws: &Workspace, checkout: &Path, name: &str) -> bool {
    recorded(ws, REWROTE, checkout, true)
        .iter()
        .any(|n| n == name)
}

/// Forgets the reviewer rewrites, once the change that carries them is submitted.
pub(crate) fn clear_reviewer_rewrites(ws: &Workspace) {
    remove(ws, REWROTE);
}

/// The rewritten patches a stop left unreviewed that still have those bytes.
pub(super) fn unreviewed_patches(ws: &Workspace, checkout: &Path) -> Vec<String> {
    recorded(ws, PENDING, checkout, false)
}

/// On a stop: every patch rewritten and not accepted goes back to the bytes the
/// stage started with, and a patch that was new has no such bytes, so it is
/// recorded as unreviewed and a rerun refuses it (see [`refuse_unreviewed`]).
/// Returns the patches put back; `Err` when one could not be put back or
/// recorded, and the stage must then stop as needing a maintainer.
pub(super) fn settle_unreviewed(
    ctx: &RunCtx,
    stage_start: &std::collections::BTreeMap<String, Option<Vec<u8>>>,
    accepted: &std::collections::BTreeMap<String, Vec<u8>>,
) -> Result<Vec<String>, String> {
    let spec = std::fs::read_to_string(&ctx.spec_path).unwrap_or_default();
    let mut put_back = Vec::new();
    let mut failed: Vec<String> = Vec::new();
    for name in rewritten_patches(&spec, &ctx.checkout_dir, stage_start, accepted) {
        match stage_start.get(&name).and_then(|o| o.as_deref()) {
            Some(bytes) => match ap_core::fsx::write_atomic(&ctx.checkout_dir.join(&name), bytes) {
                Ok(()) => put_back.push(name),
                Err(e) => failed.push(format!("{name} could not be put back: {e}")),
            },
            None => {
                if let Err(e) = append(&ctx.ws, PENDING, &ctx.checkout_dir, &name) {
                    failed.push(e);
                }
            }
        }
    }
    if failed.is_empty() {
        Ok(put_back)
    } else {
        Err(failed.join("; "))
    }
}

/// The result a stage returns after `settle_unreviewed`: `result`, unless the
/// settling failed, which stops the package for a maintainer.
pub(super) fn settle_stop(
    ctx: &RunCtx,
    settled: &Result<Vec<String>, String>,
    result: PipelineResult,
) -> PipelineResult {
    match settled {
        Ok(_) => result,
        Err(why) => {
            UNSETTLED.with(|u| u.set(true));
            ctx.failed_as(
                FailureKind::NeedsMaintainer,
                format!("a rewritten patch is left unreviewed: {why}"),
            )
        }
    }
}

thread_local! {
    /// Set when a stage stop could not settle its rewritten patches: the turn
    /// record must then stay (see [`take_unsettled`]).
    static UNSETTLED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Whether the stage that just ended failed to settle its patches; resets the flag.
pub(super) fn take_unsettled() -> bool {
    UNSETTLED.with(|u| u.replace(false))
}

/// What `in-progress` records: the spec it is for and the declared patches (name
/// to hash, `-` for no file) as they were when the oldest unsettled turn began.
struct Record {
    spec: String,
    patches: std::collections::BTreeMap<String, String>,
}

fn read_record(ws: &Workspace) -> Option<Record> {
    let text = [ws.dir().join(IN_PROGRESS), fallback(ws, IN_PROGRESS)]
        .iter()
        .find_map(|p| std::fs::read_to_string(p).ok())?;
    let mut lines = text.lines();
    let spec = lines.next()?.strip_prefix("spec\t")?.to_string();
    let patches = lines
        .filter_map(|l| l.split_once('\t'))
        .map(|(n, h)| (n.to_string(), h.to_string()))
        .collect();
    Some(Record { spec, patches })
}

/// Records the declared patches as they are before an agent turn that may edit
/// them: their hashes in `patch-review/in-progress` (or its fallback) and a copy
/// of each under `patch-review/start/`. A record for this spec that is still
/// there is kept: it holds the oldest bytes of work that is not settled yet.
/// `Err` when it cannot be written: the turn must not run unrecorded.
pub(super) fn begin_turn(ws: &Workspace, spec_path: &Path) -> Result<(), String> {
    let me = spec_path.display().to_string();
    if read_record(ws).is_some_and(|r| r.spec == me) {
        return Ok(());
    }
    let spec = std::fs::read_to_string(spec_path).unwrap_or_default();
    let checkout = spec_path.parent().unwrap_or(Path::new("."));
    let start = ws.dir().join(START_DIR);
    let _ = std::fs::remove_dir_all(&start);
    std::fs::create_dir_all(&start).map_err(|e| format!("cannot record the patches: {e}"))?;
    let mut record = format!("spec\t{me}\n");
    for (name, bytes) in crate::pipeline::patch_stage::patch_files(&spec, checkout) {
        match bytes {
            Some(bytes) => {
                write_private(&start.join(&name), &bytes)
                    .map_err(|e| format!("cannot copy {name}: {e}"))?;
                record.push_str(&format!("{name}\t{}\n", hash_of(&bytes)));
            }
            None => record.push_str(&format!("{name}\t-\n")),
        }
    }
    write_private(&ws.dir().join(IN_PROGRESS), record.as_bytes()).or_else(|first| {
        write_private(&fallback(ws, IN_PROGRESS), record.as_bytes())
            .map_err(|second| format!("cannot record the patches ({first}; {second})"))
    })
}

/// Drops the turn record when it is for this turn's work: it stays recorded when an
/// agent turn exits (see [`TurnRecord`]) with the patches different from it.
pub(super) struct TurnRecord<'a> {
    ws: &'a Workspace,
    spec: PathBuf,
}

impl<'a> TurnRecord<'a> {
    /// Records the patches and returns the guard that settles the record when the
    /// turn is over.
    pub(super) fn begin(ws: &'a Workspace, spec: &Path) -> Result<Self, String> {
        begin_turn(ws, spec)?;
        Ok(Self {
            ws,
            spec: spec.to_path_buf(),
        })
    }
}

impl Drop for TurnRecord<'_> {
    fn drop(&mut self) {
        settle_record(self.ws, &self.spec);
    }
}

/// Forgets the turn record: its work is settled (reviewed, rolled back or put
/// back by a stage that recorded what it could not).
pub(super) fn end_turn(ws: &Workspace) {
    remove(ws, IN_PROGRESS);
    let _ = std::fs::remove_dir_all(ws.dir().join(START_DIR));
}

/// The patches that differ from the record: a changed or missing file, a patch
/// the record does not know (when it knew some), one it knew that the spec no
/// longer declares.
fn differing(record: &Record, spec_path: &Path) -> Vec<String> {
    let spec = std::fs::read_to_string(spec_path).unwrap_or_default();
    let checkout = spec_path.parent().unwrap_or(Path::new("."));
    let now = crate::pipeline::patch_stage::patch_files(&spec, checkout);
    let mut out: Vec<String> = Vec::new();
    for (name, bytes) in &now {
        let changed = match (record.patches.get(name).map(String::as_str), bytes) {
            (Some("-"), None) => false,
            (Some(hash), Some(b)) => hash != hash_of(b),
            (Some(_), None) => true,
            (None, Some(_)) => !record.patches.is_empty(),
            (None, None) => false,
        };
        if changed {
            out.push(name.clone());
        }
    }
    out.extend(
        record
            .patches
            .keys()
            .filter(|n| !now.contains_key(*n))
            .cloned(),
    );
    out
}

/// The one rule for the turn record: it is dropped when the declared patches equal
/// it, and kept when one differs. `true` when no record is left for this spec.
/// A record for another spec is not this package's and is left alone.
pub(super) fn settle_record(ws: &Workspace, spec_path: &Path) -> bool {
    let Some(record) = read_record(ws) else {
        return true;
    };
    if record.spec != spec_path.display().to_string() {
        return true;
    }
    if differing(&record, spec_path).is_empty() {
        end_turn(ws);
        return true;
    }
    false
}

/// Every file a stop leaves records in, for a message: the workspace's and the
/// fallback place, with absolute paths.
fn files_of(ws: &Workspace, file: &str) -> String {
    format!(
        "{} and {}",
        ws.dir().join(file).display(),
        fallback(ws, file).display()
    )
}

/// The stop for a patch a rerun finds rewritten and never reviewed, before any
/// stage runs: the checkout is reused across runs, so it would be built. Two
/// causes: a stop left a new patch unreviewed (`pending`), or a turn record
/// (`in-progress`) holds patches that differ now. A record that equals the
/// patches is settled and removed.
pub(super) fn refuse_unreviewed(ctx: &RunCtx) -> Option<PipelineResult> {
    let names = unreviewed_patches(&ctx.ws, &ctx.checkout_dir);
    if !names.is_empty() {
        return Some(ctx.failed_as(
            FailureKind::NeedsMaintainer,
            format!(
                "{} was rewritten by an agent and a stop left it unreviewed; read it in {}, then \
                 edit it, or accept it by removing {}",
                names.join(", "),
                ctx.checkout_dir.display(),
                files_of(&ctx.ws, PENDING)
            ),
        ));
    }
    let record = read_record(&ctx.ws)?;
    if record.spec != ctx.spec_path.display().to_string() {
        return None;
    }
    let names = differing(&record, &ctx.spec_path);
    if names.is_empty() {
        end_turn(&ctx.ws);
        return None;
    }
    Some(ctx.failed_as(
        FailureKind::NeedsMaintainer,
        format!(
            "{} differ from what they were when an agent turn began, and no review settled \
             them (the run may have ended while the turn could edit them). The patches as they \
             were are in {}; inspect them against {}, restore them or accept the changes by \
             removing {} and {}",
            names.join(", "),
            ctx.ws.dir().join(START_DIR).display(),
            ctx.checkout_dir.display(),
            ctx.ws.dir().join(IN_PROGRESS).display(),
            fallback(&ctx.ws, IN_PROGRESS).display()
        ),
    ))
}
