use super::*;

/// An archive in the scratch directory. It is a lease on the file: the file
/// is deleted when the last `Arc<ArchiveFile>` goes (the cache's own included),
/// so a path handed out stays valid for as long as its holder keeps the lease,
/// whatever the cache or [`fetch_archive_to`] does meanwhile.
#[derive(Debug)]
pub struct ArchiveFile {
    path: PathBuf,
    /// Set once the file was moved away, so there is nothing to delete.
    moved: std::sync::atomic::AtomicBool,
}

impl ArchiveFile {
    pub(super) fn new(path: PathBuf) -> Arc<Self> {
        Arc::new(ArchiveFile {
            path,
            moved: Default::default(),
        })
    }
}

impl PartialEq for ArchiveFile {
    fn eq(&self, other: &Self) -> bool {
        self.path == other.path
    }
}

impl Eq for ArchiveFile {}

impl std::ops::Deref for ArchiveFile {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.path
    }
}

impl AsRef<Path> for ArchiveFile {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

impl Drop for ArchiveFile {
    fn drop(&mut self) {
        if !self.moved.load(Ordering::SeqCst) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Archives fetched this run, by URL. The probe downloads the archive it
/// inspects; the spec generator takes that copy rather than fetching it again.
#[derive(Default)]
pub(super) struct ArchiveCache {
    pub(super) files: std::collections::HashMap<String, Arc<ArchiveFile>>,
    /// URLs that answered 404, so they are not asked for again this run.
    pub(super) missing: std::collections::HashSet<String>,
    /// The latest other failure per URL and when it happened, so callers
    /// queued behind a failed download take its result rather than each
    /// repeating the download and its retries.
    pub(super) failed: std::collections::HashMap<String, (std::time::Instant, FetchFailure)>,
    /// One lock per URL being fetched, so concurrent callers for the same URL
    /// wait for one download instead of racing to write it. An entry lives
    /// only while someone holds or waits for it (see [`Slot`]).
    pub(super) in_flight: std::collections::HashMap<String, Arc<SlotState>>,
}

/// How long a failed download other than a 404 is reused for the same URL.
pub(super) const FAILURE_REUSE: std::time::Duration = std::time::Duration::from_secs(30);

pub(super) fn archive_cache() -> &'static Mutex<ArchiveCache> {
    static CACHE: OnceLock<Mutex<ArchiveCache>> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// The state behind a URL's in-flight lock: whether someone has the turn.
#[derive(Default)]
pub(super) struct SlotState {
    busy: Mutex<bool>,
    freed: std::sync::Condvar,
}

/// A turn on a URL's in-flight lock. Dropping it passes the turn on and
/// removes the map entry when nobody else is waiting on it.
pub(super) struct Slot {
    url: String,
    /// Taken in `drop`, so the handle is released before the map is checked.
    state: Option<Arc<SlotState>>,
}

impl Slot {
    /// Wait for this URL's turn.
    pub(super) fn take(url: &str) -> Slot {
        let state = archive_cache()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .in_flight
            .entry(url.to_string())
            .or_default()
            .clone();
        let mut busy = state.busy.lock().unwrap_or_else(|e| e.into_inner());
        while *busy {
            busy = state.freed.wait(busy).unwrap_or_else(|e| e.into_inner());
        }
        *busy = true;
        drop(busy);
        Slot {
            url: url.to_string(),
            state: Some(state),
        }
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        let Some(state) = self.state.take() else {
            return;
        };
        *state.busy.lock().unwrap_or_else(|e| e.into_inner()) = false;
        state.freed.notify_one();
        // Our handle is released before the check, so a waiter that still
        // holds one keeps the entry, and the last one out removes it.
        drop(state);
        let mut cache = archive_cache().lock().unwrap_or_else(|e| e.into_inner());
        // Only the map's handle is left: nobody holds or awaits this slot.
        // Waiters clone theirs under this same lock.
        if cache
            .in_flight
            .get(&self.url)
            .is_some_and(|entry| Arc::strong_count(entry) <= 1)
        {
            cache.in_flight.remove(&self.url);
        }
    }
}

/// Where archives are downloaded: under the work directory's cache when one was
/// set (`/tmp` may be a tmpfs, which is memory), else the private directory.
pub(super) static SCRATCH_ROOT: OnceLock<PathBuf> = OnceLock::new();

/// Put archive scratch space under `dir`. Set once at startup; later calls are
/// ignored.
pub fn set_scratch_dir(dir: PathBuf) {
    let _ = SCRATCH_ROOT.set(dir);
}

/// The per-process archive directory (mode 0700, created once). Removed by
/// [`remove_scratch_dir`] at exit; directories of earlier runs that were killed
/// before they could are swept when they are a day old.
pub(super) fn archive_dir() -> Result<&'static Path, FetchFailure> {
    static DIR: OnceLock<Result<PathBuf, String>> = OnceLock::new();
    DIR.get_or_init(|| {
        let err = |e: std::io::Error| e.to_string();
        let root = match SCRATCH_ROOT.get() {
            Some(root) => root.clone(),
            None => crate::proc::private_dir().map_err(err)?.join("archives"),
        };
        std::fs::create_dir_all(&root).map_err(err)?;
        sweep_stale_dirs(&root);
        use std::os::unix::fs::PermissionsExt;
        tempfile::Builder::new()
            .prefix("archives-")
            .permissions(std::fs::Permissions::from_mode(0o700))
            .tempdir_in(&root)
            .map(tempfile::TempDir::keep)
            .map_err(err)
    })
    .as_ref()
    .map(PathBuf::as_path)
    .map_err(|e| FetchFailure::Transport(e.clone()))
}

/// Delete `archives-*` directories in `root` untouched for a day.
pub(super) fn sweep_stale_dirs(root: &Path) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let old = entry.file_name().to_string_lossy().starts_with("archives-")
            && entry
                .metadata()
                .ok()
                .filter(|m| m.is_dir())
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age > std::time::Duration::from_secs(24 * 3600));
        if old {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// Remove this process's archive directory. Best effort, for process exit.
pub fn remove_scratch_dir() {
    if let Ok(dir) = archive_dir() {
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// Download `url` to `dest`, noting a 404 in the cache.
pub(super) fn download_archive(url: &str, dest: &Path) -> Result<(), FetchFailure> {
    let result = crate::http::download_file(url, dest);
    if let Err(FetchFailure::NotFound) = result {
        archive_cache()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .missing
            .insert(url.to_string());
    }
    let download = result?;
    ap_core::diag!(
        "  fetched {url} ({} bytes, sha256 {})",
        download.bytes,
        download.sha256
    );
    Ok(())
}

/// Drop `url` from the cache. Holders of the file keep it until they let go.
pub(super) fn evict_archive(url: &str) {
    let old = archive_cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .files
        .remove(url);
    drop(old);
}

/// The archive at `url` as a file in this process's scratch directory, reused
/// when it was already fetched this run. The file stays until the returned
/// lease and every other one for it are dropped.
pub fn fetch_archive(url: &str) -> Result<Arc<ArchiveFile>, FetchFailure> {
    let cached = || {
        let cache = archive_cache().lock().unwrap_or_else(|e| e.into_inner());
        if cache.missing.contains(url) {
            return Some(Err(FetchFailure::NotFound));
        }
        cache
            .files
            .get(url)
            .filter(|f| f.is_file())
            .map(|f| Ok(Arc::clone(f)))
    };
    if let Some(hit) = cached() {
        return hit;
    }
    // Callers wanting the same URL queue on its slot: the first downloads, the
    // rest find the result in the cache. Other URLs proceed in parallel.
    let _turn = Slot::take(url);
    if let Some(hit) = cached() {
        return hit;
    }
    let recent_failure = archive_cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .failed
        .get(url)
        .filter(|(at, _)| at.elapsed() < FAILURE_REUSE)
        .map(|(_, failure)| failure.clone());
    if let Some(failure) = recent_failure {
        return Err(failure);
    }
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let dir = archive_dir()?;
    let base = url
        .split(['?', '#'])
        .next()
        .and_then(|u| u.rsplit('/').next())
        .filter(|b| !b.is_empty())
        .unwrap_or("archive");
    let path = dir.join(format!("{}-{base}", NEXT.fetch_add(1, Ordering::Relaxed)));
    if let Err(failure) = download_archive(url, &path) {
        if failure != FetchFailure::NotFound {
            archive_cache()
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .failed
                .insert(
                    url.to_string(),
                    (std::time::Instant::now(), failure.clone()),
                );
        }
        return Err(failure);
    }
    let file = ArchiveFile::new(path);
    archive_cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .files
        .insert(url.to_string(), Arc::clone(&file));
    Ok(file)
}

/// Put the archive at `url` at `dest`: the copy the probe fetched when there is
/// one, otherwise a fresh download. The cached file is moved there when nobody
/// else holds it (so it is not kept twice), and hard-linked or copied when a
/// reader still has it: the path they were given stays valid.
pub fn fetch_archive_to(url: &str, dest: &Path) -> Result<(), FetchFailure> {
    // The URL's turn, so this neither races a download in progress nor starts
    // a second one.
    let _turn = Slot::take(url);
    let cached = {
        let mut cache = archive_cache().lock().unwrap_or_else(|e| e.into_inner());
        if cache.missing.contains(url) {
            return Err(FetchFailure::NotFound);
        }
        cache.files.remove(url)
    };
    if let Some(file) = cached.filter(|f| f.is_file()) {
        // Off the cache nobody can take a new lease, so the count only falls.
        match Arc::try_unwrap(file) {
            Ok(file) => {
                // A rename moves no data; it fails across filesystems (EXDEV),
                // where a copy is the only way.
                if std::fs::rename(&file.path, dest).is_ok() {
                    file.moved.store(true, Ordering::SeqCst);
                    return Ok(());
                }
                if std::fs::copy(&file.path, dest).is_ok() {
                    return Ok(());
                }
            }
            Err(shared) => {
                let _ = std::fs::remove_file(dest);
                if std::fs::hard_link(&shared.path, dest).is_ok()
                    || std::fs::copy(&shared.path, dest).is_ok()
                {
                    return Ok(());
                }
            }
        }
    }
    download_archive(url, dest)
}
