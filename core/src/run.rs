use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::event::{RunId, StepId};

/// Sanitize a name for use as a directory component.
///
/// Keeps alphanumeric, `-`, `_`, `.`; replaces everything else with `_`.
///
/// Then rejects path-traversal shapes: a bare `.` (current directory) and
/// `..` (parent directory) are both path components the filesystem will
/// resolve relative to the caller's cwd, so an attacker-controlled or
/// mis-typed name along those lines could land artifacts outside the
/// intended run directory. Any purely-dot result is replaced with `_` so
/// the sanitized name is always a plain, in-place directory component.
/// Empty inputs are mapped the same way.
///
/// Earlier review probe: `..` survived sanitization because `.` is in the
/// permitted character class and no post-filter check rejected the fully-
/// dot result.
pub fn sanitize_name(name: &str) -> String {
    let sanitized: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if sanitized.is_empty() || sanitized.chars().all(|c| c == '.') {
        return "_".to_string();
    }
    sanitized
}

/// Distinguishes IDs minted in the same nanosecond (or on a coarse clock)
/// by one process.
static RUN_ID_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Generate a run ID: `YYYYMMDD-HHMMSS-<9 hex nanos>-<pid hex>-<counter hex>`.
///
/// The full sub-second nanosecond value, the process id and a per-process
/// counter keep two runs (even concurrent ones) from sharing an ID.
pub fn new_run_id() -> RunId {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    let s = secs % 60;
    let m = (secs / 60) % 60;
    let h = (secs / 3600) % 24;
    let days = secs / 86400;
    let (y, mo, d) = days_to_ymd(days);
    let sub = now.subsec_nanos();
    let pid = std::process::id();
    let n = RUN_ID_COUNTER.fetch_add(1, Ordering::Relaxed);
    RunId(format!(
        "{y:04}{mo:02}{d:02}-{h:02}{m:02}{s:02}-{sub:08x}-{pid:x}-{n:x}"
    ))
}

fn days_to_ymd(mut days: u64) -> (u64, u64, u64) {
    let mut y = 1970u64;
    loop {
        let ydays = if is_leap(y) { 366 } else { 365 };
        if days < ydays {
            break;
        }
        days -= ydays;
        y += 1;
    }
    let month_days: [u64; 12] = [
        31,
        if is_leap(y) { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut mo = 1u64;
    for &md in &month_days {
        if days < md {
            break;
        }
        days -= md;
        mo += 1;
    }
    (y, mo, days + 1)
}

fn is_leap(y: u64) -> bool {
    (y.is_multiple_of(4) && !y.is_multiple_of(100)) || y.is_multiple_of(400)
}

/// The immutable directory for one run: `<work-dir>/runs/<run-id>/`.
pub struct RunDir {
    pub path: PathBuf,
    pub run_id: RunId,
    /// Shared step counters, keyed by sanitized rpm name. The outer build loop calls
    /// `pkg(rpm_name)` once per retry — if each call allocated a fresh
    /// counter, two attempts of the same package would both allocate
    /// `001-build/`, `002-fix/`, etc., and the second attempt's log would
    /// overwrite the first. Sharing an `Arc<AtomicU32>` per rpm name
    /// across all `PkgRunDir`s for that name means retries get the next
    /// number (`003-build/`, …) and no attempt's artifacts can be lost.
    pkg_counters: Mutex<HashMap<String, Arc<AtomicU32>>>,
}

impl RunDir {
    /// Allocate a new, empty run directory.
    pub fn allocate(work_dir: &Path) -> io::Result<Self> {
        let runs = work_dir.join("runs");
        fs::create_dir_all(&runs)?;
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
        // `create_dir` (not `create_dir_all`) so an ID that is already taken
        // is an error to retry, never a silently shared run directory.
        for _ in 0..16 {
            let run_id = new_run_id();
            let path = runs.join(&run_id.0);
            match builder.create(&path) {
                Ok(()) => {
                    return Ok(Self {
                        path,
                        run_id,
                        pkg_counters: Mutex::new(HashMap::new()),
                    });
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not allocate a unique run directory",
        ))
    }

    pub fn jsonl(&self) -> PathBuf {
        self.path.join("run.jsonl")
    }

    pub fn summary(&self) -> PathBuf {
        self.path.join("run.json")
    }

    pub fn pkg(&self, rpm_name: &str) -> PkgRunDir {
        // Key by the sanitized name: it is what names the directory, so two
        // rpm names that sanitize alike must share one counter.
        let dir_name = sanitize_name(rpm_name);
        let counter = self
            .pkg_counters
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(dir_name.clone())
            .or_insert_with(|| Arc::new(AtomicU32::new(0)))
            .clone();
        PkgRunDir {
            path: self.path.join(dir_name),
            counter,
        }
    }
}

/// Per-package directory within a run: `<run-dir>/<rpm-name>/`.
pub struct PkgRunDir {
    pub path: PathBuf,
    /// Shared with every other `PkgRunDir` for the same rpm name in the
    /// same `RunDir` — see `RunDir::pkg_counters`'s doc comment for why
    /// shared across retries matters.
    counter: Arc<AtomicU32>,
}

impl PkgRunDir {
    /// Ensure the package directory exists.
    pub fn ensure(&self) -> io::Result<()> {
        fs::create_dir_all(&self.path)
    }

    /// Allocate the next numbered step directory: `<pkg-dir>/<NNN>-<label>/`.
    ///
    /// Uses `create_dir` (not `create_dir_all`) so an existing step dir is
    /// never reused: reusing one lets a later attempt's `build.log`
    /// overwrite an earlier one's. If the next number is already taken (a
    /// stray directory left behind), the number is skipped and the next
    /// free one is used, up to `MAX_TRIES` candidates. Combined with
    /// `RunDir::pkg_counters` sharing the counter across retries, two
    /// attempts of the same package get distinct numbered dirs
    /// (`001-build/`, `002-build/`) rather than colliding.
    pub fn step(&self, label: &str) -> io::Result<StepDir> {
        const MAX_TRIES: usize = 256;
        let label = sanitize_name(label);
        for _ in 0..MAX_TRIES {
            // The counter holds the last allocated number. Step dirs of one
            // package are allocated sequentially, so load-then-bump is
            // race-free here; `fetch_max` keeps the counter monotonic.
            let n = self.counter.load(Ordering::SeqCst) + 1;
            let path = self.path.join(format!("{n:03}-{label}"));
            match fs::create_dir(&path) {
                Ok(()) => {
                    self.counter.fetch_max(n, Ordering::SeqCst);
                    return Ok(StepDir {
                        path,
                        id: StepId(n - 1),
                    });
                }
                // The number is taken: move the counter past it so the next
                // try (and every later call) does not compute the same name.
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                    self.counter.fetch_max(n, Ordering::SeqCst);
                }
                // The parent directory may not exist yet (callers often skip
                // calling `ensure()`); let that case through so it isn't a
                // regression.
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    fs::create_dir_all(&path)?;
                    self.counter.fetch_max(n, Ordering::SeqCst);
                    return Ok(StepDir {
                        path,
                        id: StepId(n - 1),
                    });
                }
                // Any other error: fail without burning the number.
                Err(e) => return Err(e),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("could not allocate a free step directory for {label:?}"),
        ))
    }
}

/// One numbered step directory within a package run dir.
pub struct StepDir {
    pub path: PathBuf,
    pub id: StepId,
}

impl StepDir {
    pub fn file(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    #[test]
    fn run_directory_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let work = tempfile::tempdir().unwrap();
        let run = super::RunDir::allocate(work.path()).unwrap();
        let mode = std::fs::metadata(&run.path).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "{mode:o}");
    }

    use super::*;

    #[test]
    fn consecutive_allocations_get_distinct_ids_and_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let a = RunDir::allocate(tmp.path()).unwrap();
        let b = RunDir::allocate(tmp.path()).unwrap();
        assert_ne!(a.run_id.0, b.run_id.0);
        assert_ne!(a.path, b.path);
    }

    /// Earlier review probe: `..` survived sanitization because the pre-fix
    /// character filter allowed `.` and had no post-filter check against
    /// purely-dot results. A `..` directory component passed to any join
    /// would resolve to the parent dir and escape the intended scope.
    #[test]
    fn sanitize_name_rejects_dot_and_dotdot_path_components() {
        assert_eq!(sanitize_name(".."), "_");
        assert_eq!(sanitize_name("."), "_");
        assert_eq!(sanitize_name("..."), "_"); // multi-dot too
        assert_eq!(sanitize_name(""), "_");
    }

    #[test]
    fn names_that_sanitize_alike_share_one_counter() {
        let tmp = tempfile::tempdir().unwrap();
        let run = RunDir::allocate(tmp.path()).unwrap();
        let a = run.pkg("home:foo");
        let b = run.pkg("home_foo");
        assert_eq!(a.path, b.path);
        a.step("build").unwrap();
        b.step("build").unwrap();
    }

    #[test]
    fn sanitize_name_keeps_an_ordinary_name_untouched() {
        assert_eq!(sanitize_name("python-foo"), "python-foo");
        assert_eq!(sanitize_name("ruamel.yaml"), "ruamel.yaml");
        assert_eq!(sanitize_name("home:you:python"), "home_you_python");
    }

    #[test]
    fn sanitize_name_keeps_a_dot_inside_a_name() {
        // A name like `backports.zstd` still passes — the rejection is
        // only for a name that is NOTHING BUT dots.
        assert_eq!(sanitize_name("backports.zstd"), "backports.zstd");
    }

    /// Earlier review probe: two attempts of the same package allocated
    /// the same `001-build/build.log` because each `RunDir::pkg()` call
    /// handed back a fresh counter starting at zero. Retries must get
    /// distinct numbered step dirs so the first attempt's artifacts
    /// survive long enough to be inspected.
    #[test]
    fn step_counters_are_shared_across_pkg_calls_for_the_same_rpm_name() {
        let tmp = tempfile::tempdir().unwrap();
        let run = RunDir::allocate(tmp.path()).unwrap();

        // First attempt
        let p1 = run.pkg("python-foo");
        p1.ensure().unwrap();
        let s1 = p1.step("build").unwrap();
        assert!(s1.path.ends_with("001-build"));

        // Retry: a new PkgRunDir handle for the same package. The counter
        // is shared, so the next step is 002, not 001 — the first attempt's
        // 001-build/ directory is preserved.
        let p2 = run.pkg("python-foo");
        let s2 = p2.step("build").unwrap();
        assert!(s2.path.ends_with("002-build"), "got {:?}", s2.path);
        assert_ne!(
            s1.path, s2.path,
            "retry must not reuse the first attempt's dir"
        );
    }

    /// Two different packages must have independent counters — a build of
    /// `python-foo` must not consume a step number that then shifts
    /// `python-bar`'s indices.
    #[test]
    fn step_counters_are_per_rpm_name() {
        let tmp = tempfile::tempdir().unwrap();
        let run = RunDir::allocate(tmp.path()).unwrap();

        let foo = run.pkg("python-foo");
        foo.ensure().unwrap();
        let _ = foo.step("build").unwrap();
        let _ = foo.step("fix").unwrap();

        let bar = run.pkg("python-bar");
        bar.ensure().unwrap();
        let s = bar.step("build").unwrap();
        assert!(s.path.ends_with("001-build"), "got {:?}", s.path);
    }

    /// `step()` must never hand back a directory that already exists — an
    /// existing `001-build/` keeps its `build.log` untouched, and the
    /// allocation moves on to the next free number.
    #[test]
    fn step_skips_an_existing_directory_instead_of_reusing_it() {
        let tmp = tempfile::tempdir().unwrap();
        let run = RunDir::allocate(tmp.path()).unwrap();

        let p1 = run.pkg("python-foo");
        p1.ensure().unwrap();
        let s = p1.step("build").unwrap();
        std::fs::write(s.file("build.log"), b"first attempt").unwrap();

        // Simulate counter corruption / external interference: the counter
        // is rewound so the next allocation would target 001-build again.
        p1.counter.store(0, Ordering::SeqCst);
        let s2 = p1.step("build").unwrap();
        assert!(s2.path.ends_with("002-build"), "got {:?}", s2.path);
        assert_ne!(s.path, s2.path);
        assert_eq!(
            std::fs::read(s.file("build.log")).unwrap(),
            b"first attempt",
            "the existing step dir must not be touched"
        );

        // The counter has advanced past the taken number, so later calls
        // keep making progress instead of failing forever.
        let s3 = p1.step("fix").unwrap();
        assert!(s3.path.ends_with("003-fix"), "got {:?}", s3.path);
    }

    /// A stray directory must not wedge the package: the counter advances
    /// past every taken number, so the next call succeeds.
    #[test]
    fn step_does_not_wedge_after_a_stray_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let run = RunDir::allocate(tmp.path()).unwrap();

        let p = run.pkg("python-foo");
        p.ensure().unwrap();
        std::fs::create_dir(p.path.join("001-build")).unwrap();
        std::fs::create_dir(p.path.join("002-build")).unwrap();

        let s = p.step("build").unwrap();
        assert!(s.path.ends_with("003-build"), "got {:?}", s.path);
        let s2 = p.step("build").unwrap();
        assert!(s2.path.ends_with("004-build"), "got {:?}", s2.path);
    }
}
