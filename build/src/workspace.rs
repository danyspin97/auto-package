//! Per-package durable workspace: `<work-dir>/state/<rpm-pkg>/`.
//!
//! The durable workspace survives runs. Run-specific artifacts live in
//! `RunDir`/`PkgRunDir`/`StepDir` from ap-core.
//!
//! The osc working copy (`<work-dir>/<project>/<rpm-name>/`) holds ONLY
//! the spec and the tarball so `osc status` stays clean.

use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use ap_core::event::{Event, StepId};
use ap_core::fsx::{free_bytes, wait_for_space};
use ap_core::run::sanitize_name;
use ap_core::sink::Reporter;

const GIB: u64 = 1 << 30;

/// Free space a package needs on the work directory's filesystem before its
/// heavy stages start, in bytes. 0 turns the guard off. Set once at startup
/// from `--min-free-gb`. Off in this crate's own tests, whose pipeline runs on
/// whatever disk the tests happen to have and must not wait ten minutes for it.
static MIN_FREE_BYTES: AtomicU64 = AtomicU64::new(if cfg!(test) { 0 } else { 3 * GIB });

/// How often, and for how long, a package waits for the disk to recover.
const DISK_WAIT_INTERVAL: Duration = Duration::from_secs(30);
const DISK_WAIT_LIMIT: Duration = Duration::from_secs(600);

pub fn set_min_free_gb(gb: u64) {
    MIN_FREE_BYTES.store(gb.saturating_mul(GIB), Ordering::Relaxed);
}

fn gib(bytes: u64) -> String {
    format!("{:.1} GiB", bytes as f64 / GIB as f64)
}

/// Hold a package back while the filesystem of `dir` has less than the minimum
/// free, warning every check so the TUI shows why it is idle. When the disk does
/// not recover within ten minutes the error says `No space left on device`, so
/// the ENOSPC handling and retry that already exist apply to it.
pub fn await_disk_space(
    dir: &Path,
    reporter: &dyn Reporter,
    step: Option<StepId>,
) -> Result<(), String> {
    await_disk_space_with(
        MIN_FREE_BYTES.load(Ordering::Relaxed),
        || free_bytes(dir),
        |d| {
            if crate::proc::shutting_down() {
                return false;
            }
            std::thread::sleep(d);
            true
        },
        DISK_WAIT_INTERVAL,
        DISK_WAIT_LIMIT,
        reporter,
        step,
    )
}

fn await_disk_space_with(
    min_bytes: u64,
    free: impl FnMut() -> Option<u64>,
    sleep: impl FnMut(Duration) -> bool,
    interval: Duration,
    limit: Duration,
    reporter: &dyn Reporter,
    step: Option<StepId>,
) -> Result<(), String> {
    if min_bytes == 0 {
        return Ok(());
    }
    wait_for_space(
        min_bytes,
        interval,
        limit,
        free,
        |now| {
            reporter.report(&Event::Warning {
                step,
                message: format!(
                    "waiting for disk space: {} free, need {}",
                    gib(now),
                    gib(min_bytes)
                ),
            });
        },
        sleep,
    )
    .map_err(|now| {
        format!(
            "No space left on device: {} free after waiting {} minutes, need {}; \
             free space under the work directory (or use --work-dir on a larger disk) \
             and run again",
            gib(now),
            limit.as_secs() / 60,
            gib(min_bytes)
        )
    })
}

/// One line for the start of a run when the work directory's filesystem is
/// short of room for a few big packages. `None` when there is enough, the guard
/// is off or the free space is unknown.
pub fn low_space_warning(dir: &Path) -> Option<String> {
    let min = MIN_FREE_BYTES.load(Ordering::Relaxed);
    let free = free_bytes(dir)?;
    low_space_message(dir, free, min)
}

fn low_space_message(dir: &Path, free: u64, min_bytes: u64) -> Option<String> {
    (min_bytes > 0 && free < 10 * GIB).then(|| {
        format!(
            "only {} free on the filesystem of {}; large wheels can need several GiB each. \
             Use --work-dir to put the work directory on a larger disk.",
            gib(free),
            dir.display()
        )
    })
}

/// Resolve `.` and `..` textually, without touching the file system (the link
/// target may already be gone when this runs).
fn lexically_normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            c => out.push(c),
        }
    }
    out
}

pub struct Workspace {
    dir: PathBuf,
}

impl Workspace {
    /// Open (creating if absent) the durable workspace for `rpm_pkg`.
    /// The package name is sanitized before joining the path.
    pub fn open(state_root: &Path, rpm_pkg: &str) -> std::io::Result<Self> {
        let dir = state_root.join(sanitize_name(rpm_pkg));
        fs::create_dir_all(&dir)?;
        Ok(Self { dir })
    }

    /// Open the workspace of the first of `names` that already has one, without
    /// creating a directory for a name nothing was ever built under (a read
    /// path asking about a dependency by its RPM name, `ffmpeg-8`, must not
    /// leave `state/python-ffmpeg-8` behind).
    pub fn find_existing(state_root: &Path, names: &[&str]) -> Option<Self> {
        names.iter().find_map(|n| {
            let dir = state_root.join(sanitize_name(n));
            dir.is_dir().then_some(Self { dir })
        })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Build log path (legacy — new code writes logs into StepDir).
    pub fn build_log(&self) -> PathBuf {
        self.dir.join("build.log")
    }

    /// rpmspec --parse stderr log (legacy).
    pub fn rpmspec_log(&self) -> PathBuf {
        self.dir.join("rpmspec.txt")
    }

    /// Directory where the sdist tarball is extracted.
    pub fn sdist_dir(&self) -> PathBuf {
        self.dir.join("sdist")
    }

    /// Append-only attempt journal read by the agent.
    pub fn notes(&self) -> PathBuf {
        self.dir.join("notes.md")
    }

    /// Durable state JSON: last built version, last outcome.
    pub fn state_json(&self) -> PathBuf {
        self.dir.join("state.json")
    }

    /// Create or refresh the `spec` symlink pointing at `spec_path` (absolute).
    /// The link is swapped in one rename, so a reader never sees it missing and
    /// a failure leaves the previous link in place.
    #[cfg(unix)]
    pub fn link_spec(&self, spec_path: &Path) -> std::io::Result<()> {
        ap_core::fsx::replace_symlink(spec_path, &self.dir.join("spec"))
    }

    /// Records which tarball `sdist_dir` was extracted from.
    fn sdist_stamp(&self) -> PathBuf {
        self.dir.join("sdist.stamp")
    }

    /// `name size mtime_ns` of a tarball, the identity the stamp records. The
    /// mtime tells a regenerated tarball with the same name and size apart.
    fn tarball_identity(tarball: &Path) -> Option<String> {
        let name = tarball.file_name()?.to_string_lossy().into_owned();
        let meta = fs::metadata(tarball).ok()?;
        let len = meta.len();
        let mtime = meta
            .modified()
            .ok()?
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_nanos();
        Some(format!("{name} {len} {mtime}"))
    }

    /// True when sdist_dir holds a completed extraction of this very tarball.
    /// A different tarball (version bump, regenerated source) or an extraction
    /// that never finished (no stamp) reads as not extracted.
    pub fn sdist_extracted_from(&self, tarball: &Path) -> bool {
        let dir = self.sdist_dir();
        let non_empty = fs::read_dir(&dir)
            .map(|mut d| d.next().is_some())
            .unwrap_or(false);
        non_empty
            && Self::tarball_identity(tarball)
                .is_some_and(|id| fs::read_to_string(self.sdist_stamp()).is_ok_and(|s| s == id))
    }

    /// Remove any previous extraction so a new one starts from an empty dir.
    pub fn clear_sdist(&self) -> std::io::Result<()> {
        let _ = fs::remove_file(self.sdist_stamp());
        match fs::remove_dir_all(self.sdist_dir()) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }

    /// Record a finished extraction of `tarball` into `sdist_dir`.
    pub fn mark_sdist_extracted(&self, tarball: &Path) -> std::io::Result<()> {
        match Self::tarball_identity(tarball) {
            Some(id) => fs::write(self.sdist_stamp(), id),
            None => Ok(()),
        }
    }

    /// After a package is Built, free the unpacked tree of a prebuilt wheel
    /// (hundreds of MB for the NVIDIA wheels) and say so. A failure only warns.
    pub fn release_wheel_tree(&self, reporter: &dyn Reporter) {
        match self.prune_wheel_extraction() {
            Ok(0) => {}
            Ok(freed) => reporter.report(&Event::Progress {
                step: None,
                message: format!("freed {} of the unpacked wheel", gib(freed)),
            }),
            Err(e) => reporter.report(&Event::Warning {
                step: None,
                message: format!("not removing the unpacked wheel: {e}"),
            }),
        }
    }

    /// Remove the extraction in `sdist_dir`, its stamp and the `source` link
    /// into it, but only when the stamp says the tree is a wheel's (a source
    /// tarball's tree is what an agent reads when it fixes a build). Returns the
    /// bytes freed, 0 when nothing was removed. The directory must resolve to a
    /// place inside this state dir: a link out of it is refused and nothing is
    /// deleted. Spec, notes and state.json are never touched. The next run that
    /// needs the tree finds no stamp and extracts the wheel again.
    pub fn prune_wheel_extraction(&self) -> std::io::Result<u64> {
        let stamp = self.sdist_stamp();
        let is_wheel = fs::read_to_string(&stamp)
            .ok()
            .and_then(|s| {
                // Stamp is `name size mtime_ns`; the name is everything before the last two fields.
                s.rsplitn(3, ' ').nth(2).map(|name| name.ends_with(".whl"))
            })
            .unwrap_or(false);
        if !is_wheel {
            return Ok(0);
        }
        let sdist = self.sdist_dir();
        let state = fs::canonicalize(&self.dir)?;
        let tree = match fs::canonicalize(&sdist) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let _ = fs::remove_file(&stamp);
                return Ok(0);
            }
            Err(e) => return Err(e),
        };
        if tree != state.join("sdist") {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!(
                    "{} resolves to {}, outside this state directory",
                    sdist.display(),
                    tree.display()
                ),
            ));
        }
        let freed = walkdir::WalkDir::new(&tree)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter_map(|e| e.metadata().ok())
            .filter(|m| m.is_file())
            .map(|m| m.len())
            .sum();
        // The stamp goes first: a half-removed tree must never read as extracted.
        let _ = fs::remove_file(&stamp);
        fs::remove_dir_all(&tree)?;
        let link = self.dir.join("source");
        if let Ok(target) = fs::read_link(&link) {
            // A relative target is relative to the link's own directory, the state dir.
            let resolved = lexically_normalize(&self.dir.join(&target));
            if resolved.starts_with(&sdist) || resolved.starts_with(&tree) {
                let _ = fs::remove_file(&link);
            }
        }
        Ok(freed)
    }

    /// Update `latest` symlink to point at a per-run package directory, in one
    /// rename so a failure leaves the previous link in place.
    #[cfg(unix)]
    pub fn link_latest(&self, run_pkg_dir: &Path) -> std::io::Result<()> {
        ap_core::fsx::replace_symlink(run_pkg_dir, &self.dir.join("latest"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_existing_never_creates_a_state_dir() {
        let tmp = tempfile::tempdir().unwrap();
        fs::create_dir(tmp.path().join("ffmpeg-8")).unwrap();
        assert!(Workspace::find_existing(tmp.path(), &["python-ffmpeg-8", "nope"]).is_none());
        let ws = Workspace::find_existing(tmp.path(), &["python-ffmpeg-8", "ffmpeg-8"]).unwrap();
        assert!(ws.dir().ends_with("ffmpeg-8"));
        assert!(!tmp.path().join("python-ffmpeg-8").exists());
    }

    #[test]
    fn sdist_is_extracted_only_for_the_tarball_it_came_from() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = Workspace::open(tmp.path(), "python-foo").unwrap();
        let old = tmp.path().join("foo-1.0.tar.gz");
        let new = tmp.path().join("foo-1.1.tar.gz");
        fs::write(&old, b"old").unwrap();
        fs::write(&new, b"newer").unwrap();

        assert!(!ws.sdist_extracted_from(&old));
        fs::create_dir_all(ws.sdist_dir().join("foo-1.0")).unwrap();
        // Extraction not finished: no stamp yet.
        assert!(!ws.sdist_extracted_from(&old));
        ws.mark_sdist_extracted(&old).unwrap();
        assert!(ws.sdist_extracted_from(&old));
        assert!(!ws.sdist_extracted_from(&new));

        ws.clear_sdist().unwrap();
        assert!(!ws.sdist_dir().exists());
        assert!(!ws.sdist_extracted_from(&old));
        ws.clear_sdist().unwrap();
    }

    #[test]
    fn a_regenerated_tarball_with_same_name_and_size_is_not_the_extracted_one() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = Workspace::open(tmp.path(), "python-foo").unwrap();
        let tarball = tmp.path().join("foo-1.0.tar.gz");
        fs::write(&tarball, b"same").unwrap();
        fs::create_dir_all(ws.sdist_dir().join("foo-1.0")).unwrap();
        ws.mark_sdist_extracted(&tarball).unwrap();
        assert!(ws.sdist_extracted_from(&tarball));

        // Rewrite with the same length but a later mtime.
        fs::write(&tarball, b"that").unwrap();
        let later = fs::metadata(&tarball).unwrap().modified().unwrap()
            + Duration::from_secs(60);
        fs::File::options()
            .write(true)
            .open(&tarball)
            .unwrap()
            .set_modified(later)
            .unwrap();
        assert!(!ws.sdist_extracted_from(&tarball));
    }

    fn wheel_workspace(tmp: &Path, stamp_name: &str) -> Workspace {
        let ws = Workspace::open(tmp, "python-foo").unwrap();
        fs::create_dir_all(ws.sdist_dir().join("foo.dist-info")).unwrap();
        fs::write(
            ws.sdist_dir().join("foo.dist-info/METADATA"),
            vec![0u8; 2048],
        )
        .unwrap();
        let tarball = tmp.join(stamp_name);
        fs::write(&tarball, b"abc").unwrap();
        ws.mark_sdist_extracted(&tarball).unwrap();
        fs::write(ws.notes(), "notes").unwrap();
        fs::write(ws.state_json(), "{}").unwrap();
        fs::write(tmp.join("spec.real"), "spec").unwrap();
        ws.link_spec(&tmp.join("spec.real")).unwrap();
        std::os::unix::fs::symlink(ws.sdist_dir(), ws.dir().join("source")).unwrap();
        ws
    }

    #[test]
    fn pruning_a_wheel_extraction_removes_the_tree_stamp_and_link_only() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = wheel_workspace(tmp.path(), "foo-1.0-py3-none-any.whl");
        assert_eq!(ws.prune_wheel_extraction().unwrap(), 2048);
        assert!(!ws.sdist_dir().exists());
        assert!(!ws.sdist_stamp().exists());
        assert!(ws.dir().join("source").symlink_metadata().is_err());
        assert!(ws.notes().exists() && ws.state_json().exists());
        assert!(ws.dir().join("spec").symlink_metadata().is_ok());
        assert!(tmp.path().join("spec.real").exists());
        // Pruned means "not extracted": the next run extracts again.
        let wheel = tmp.path().join("foo-1.0-py3-none-any.whl");
        assert!(!ws.sdist_extracted_from(&wheel));
        assert_eq!(ws.prune_wheel_extraction().unwrap(), 0);
    }

    #[test]
    fn pruning_removes_a_relative_source_link_into_the_tree() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = wheel_workspace(tmp.path(), "foo-1.0-py3-none-any.whl");
        fs::remove_file(ws.dir().join("source")).unwrap();
        std::os::unix::fs::symlink("sdist", ws.dir().join("source")).unwrap();
        assert_eq!(ws.prune_wheel_extraction().unwrap(), 2048);
        assert!(ws.dir().join("source").symlink_metadata().is_err());
    }

    #[test]
    fn pruning_leaves_a_source_tree_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = wheel_workspace(tmp.path(), "foo-1.0.tar.gz");
        assert_eq!(ws.prune_wheel_extraction().unwrap(), 0);
        assert!(ws.sdist_dir().join("foo.dist-info/METADATA").exists());
        assert!(ws.sdist_stamp().exists());
    }

    #[test]
    fn pruning_refuses_a_tree_that_resolves_outside_the_state_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let ws = wheel_workspace(tmp.path(), "foo-1.0-py3-none-any.whl");
        let outside = tmp.path().join("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("precious"), "x").unwrap();
        fs::remove_dir_all(ws.sdist_dir()).unwrap();
        std::os::unix::fs::symlink(&outside, ws.sdist_dir()).unwrap();
        assert!(ws.prune_wheel_extraction().is_err());
        assert!(outside.join("precious").exists());
        assert!(ws.sdist_stamp().exists());
    }

    struct Collect(std::sync::Mutex<Vec<String>>);
    impl Reporter for Collect {
        fn report(&self, event: &Event) {
            if let Event::Warning { message, .. } = event {
                self.0.lock().unwrap().push(message.clone());
            }
        }
    }

    #[test]
    fn a_package_waits_with_warnings_then_fails_as_enospc() {
        let rep = Collect(Default::default());
        let mut reads = 0;
        let r = await_disk_space_with(
            3 * GIB,
            || {
                reads += 1;
                Some(GIB)
            },
            |_| true,
            Duration::from_secs(30),
            Duration::from_secs(90),
            &rep,
            None,
        );
        let msg = r.unwrap_err();
        assert!(msg.contains("No space left on device"), "{msg}");
        let warnings = rep.0.lock().unwrap();
        assert_eq!(warnings.len(), 3);
        assert!(warnings[0].contains("waiting for disk space: 1.0 GiB free, need 3.0 GiB"));
        assert_eq!(reads, 4);
    }

    #[test]
    fn a_package_proceeds_when_space_returns_and_zero_disables_the_guard() {
        let rep = Collect(Default::default());
        let mut readings = vec![GIB, 4 * GIB].into_iter();
        let go = await_disk_space_with(
            3 * GIB,
            || readings.next(),
            |_| true,
            Duration::from_secs(30),
            Duration::from_secs(600),
            &rep,
            None,
        );
        assert_eq!(go, Ok(()));
        assert_eq!(rep.0.lock().unwrap().len(), 1);
        let off = await_disk_space_with(
            0,
            || panic!("the disk must not be read when the guard is off"),
            |_| true,
            Duration::from_secs(30),
            Duration::from_secs(600),
            &rep,
            None,
        );
        assert_eq!(off, Ok(()));
    }

    #[test]
    fn the_start_warning_names_work_dir_below_ten_gib_only() {
        let dir = Path::new("/w");
        let msg = low_space_message(dir, 4 * GIB, 3 * GIB).unwrap();
        assert!(
            msg.contains("--work-dir") && msg.contains("4.0 GiB"),
            "{msg}"
        );
        assert!(low_space_message(dir, 10 * GIB, 3 * GIB).is_none());
        assert!(low_space_message(dir, GIB, 0).is_none());
    }
}
