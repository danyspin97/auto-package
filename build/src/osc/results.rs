//! The cache of build results: the slots, the in-flight requests and the
//! results XML of a package.

use super::*;

/// How long a project-wide `_result` document is shared between waiters.
const RESULTS_TTL: Duration = Duration::from_secs(5);

/// How long a failed fetch is served to the waiters that ask right after it,
/// so a refusing server is asked once, not once per waiter.
const RESULTS_ERROR_TTL: Duration = Duration::from_secs(1);

/// The longest a waiter sits behind another waiter's fetch before fetching
/// itself (a little over [`OSC_QUERY_TIMEOUT`]).
const RESULTS_FLIGHT_WAIT: Duration = Duration::from_secs(70);

pub(super) type ResultsKey = (String, String, String);

pub(super) enum Slot {
    /// A fetch is running; `id` tells it apart from a later takeover.
    Fetching { id: u64 },
    /// The outcome of a fetch that began at `started` and ended at `at`.
    Ready {
        started: Instant,
        at: Instant,
        result: Result<Arc<str>, ApiFailure>,
    },
}

#[derive(Default)]
struct CacheState {
    slots: HashMap<ResultsKey, Slot>,
    next_flight: u64,
    /// Bumped per project by [`ResultsCache::invalidate`]: a fetch that began
    /// under an older epoch is not stored.
    epochs: HashMap<String, u64>,
}

/// The project-wide `_result` documents, one per (project, repository, arch),
/// shared by every waiter of the process. Every OBS call is an `osc`
/// subprocess, so tens of waiters polling their own package would start tens
/// of Python interpreters per interval; here they trigger one fetch per
/// [`RESULTS_TTL`] and read their package out of the same document.
pub(super) struct ResultsCache {
    state: Mutex<CacheState>,
    done: std::sync::Condvar,
}

/// Clears a flight's `Fetching` slot if the fetch unwinds, so the waiters
/// behind it neither hang nor wait for a result that will never come.
struct Flight<'a> {
    cache: &'a ResultsCache,
    key: &'a ResultsKey,
    id: u64,
    landed: bool,
}

impl Drop for Flight<'_> {
    fn drop(&mut self) {
        if self.landed {
            return;
        }
        let mut st = self.cache.lock();
        if matches!(st.slots.get(self.key), Some(Slot::Fetching { id, .. }) if *id == self.id) {
            st.slots.remove(self.key);
        }
        drop(st);
        self.cache.done.notify_all();
    }
}

impl ResultsCache {
    pub(super) fn new() -> Self {
        ResultsCache {
            state: Mutex::new(CacheState::default()),
            done: std::sync::Condvar::new(),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, CacheState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The document for `key`, from `fetch` unless a shared one is at most
    /// `ttl` old. Waiters for a key whose fetch is running wait for it instead
    /// of starting another. `newer_than` demands a document whose fetch began
    /// at or after that instant (a waiter that has just committed cannot use
    /// what was asked before): an older one is not served, and an older fetch
    /// in flight is waited out and then replaced.
    pub(super) fn get(
        &self,
        key: &ResultsKey,
        ttl: Duration,
        newer_than: Option<Instant>,
        fetch: impl FnOnce() -> Result<String, ApiFailure>,
    ) -> Result<Arc<str>, ApiFailure> {
        let deadline = Instant::now() + RESULTS_FLIGHT_WAIT;
        let mut st = self.lock();
        let (id, started, epoch) = loop {
            let fresh_enough = |started: &Instant| newer_than.is_none_or(|t| *started >= t);
            match st.slots.get(key) {
                Some(Slot::Ready {
                    started,
                    at,
                    result,
                }) if fresh_enough(started)
                    && at.elapsed()
                        < if result.is_ok() {
                            ttl
                        } else {
                            ttl.min(RESULTS_ERROR_TTL)
                        } =>
                {
                    return result.clone();
                }
                Some(Slot::Fetching { .. }) => {
                    let left = deadline.saturating_duration_since(Instant::now());
                    if !left.is_zero() {
                        st = self
                            .done
                            .wait_timeout(st, left)
                            .unwrap_or_else(|e| e.into_inner())
                            .0;
                        continue;
                    }
                }
                _ => {}
            }
            let id = st.next_flight;
            st.next_flight += 1;
            let started = Instant::now();
            st.slots.insert(key.clone(), Slot::Fetching { id });
            break (id, started, st.epochs.get(&key.0).copied().unwrap_or(0));
        };
        drop(st);

        let mut flight = Flight {
            cache: self,
            key,
            id,
            landed: false,
        };
        let result = fetch().map(|doc| Arc::from(doc.as_str()));
        let mut st = self.lock();
        if matches!(st.slots.get(key), Some(Slot::Fetching { id: i, .. }) if *i == id) {
            if st.epochs.get(&key.0).copied().unwrap_or(0) == epoch {
                st.slots.insert(
                    key.clone(),
                    Slot::Ready {
                        started,
                        at: Instant::now(),
                        result: result.clone(),
                    },
                );
            } else {
                st.slots.remove(key);
            }
        }
        flight.landed = true;
        drop(st);
        self.done.notify_all();
        result
    }

    /// Forget `project`'s documents: its sources changed, so what was fetched
    /// so far describes the old state. A fetch in flight is not stored.
    pub(super) fn invalidate(&self, project: &str) {
        let mut st = self.lock();
        *st.epochs.entry(project.to_string()).or_insert(0) += 1;
        st.slots
            .retain(|k, slot| k.0 != project || matches!(slot, Slot::Fetching { .. }));
    }
}

pub(super) static RESULTS_CACHE: LazyLock<ResultsCache> = LazyLock::new(ResultsCache::new);

/// Every entry of the `_result` document for `pkg`'s repository/arch, when the
/// document has `pkg` in it. The project-wide document comes from `cache`;
/// one that is cut off, or lacks the package (not scheduled yet, or fetched
/// before it was added), is replaced by `fetch_package`, the document of the
/// package alone (with its multibuild flavors). A failure of the shared fetch
/// is returned as is: asking again another way meets the same refusal.
#[allow(clippy::too_many_arguments)]
pub(super) fn package_entries(
    cache: &ResultsCache,
    project: &str,
    repo: &str,
    arch: &str,
    pkg: &str,
    fresh_since: Option<Instant>,
    fetch_project: impl FnOnce() -> Result<String, ApiFailure>,
    fetch_package: impl FnOnce() -> Result<String, ApiFailure>,
) -> Result<Option<Vec<obs_status::ResultEntry>>, ApiFailure> {
    let key = (project.to_string(), repo.to_string(), arch.to_string());
    let doc = cache.get(&key, RESULTS_TTL, fresh_since, fetch_project)?;
    if let Ok(entries) = obs_status::try_parse_results(&doc)
        && obs_status::entry_in(&entries, repo, arch, pkg).is_some()
    {
        return Ok(Some(entries));
    }
    let xml = fetch_package()?;
    let entries = obs_status::parse_results(&xml);
    let found = obs_status::entry_in(&entries, repo, arch, pkg).is_some();
    Ok(found.then_some(entries))
}

/// [`package_entries`] against OBS. The names must be valid OBS names.
pub(super) fn obs_package_entries(
    project: &str,
    repo: &str,
    arch: &str,
    pkg: &str,
    fresh_since: Option<Instant>,
) -> Result<Option<Vec<obs_status::ResultEntry>>, ApiFailure> {
    // `multibuild=1` adds the package's `<pkg>:<flavor>` entries to the
    // single-package document; the project-wide one lists them anyway.
    let (project_path, package_path) = (
        format!(
            "/build/{}/_result?repository={}&arch={}",
            enc(project),
            enc(repo),
            enc(arch)
        ),
        format!(
            "/build/{}/_result?package={}&repository={}&arch={}&multibuild=1",
            enc(project),
            enc(pkg),
            enc(repo),
            enc(arch)
        ),
    );
    package_entries(
        &RESULTS_CACHE,
        project,
        repo,
        arch,
        pkg,
        fresh_since,
        || osc_api_checked(&project_path),
        || osc_api_checked(&package_path),
    )
}

/// `fresh_since`: only a document fetched after that instant may be used
/// (see [`ResultsCache::get`]); `None` accepts one a few seconds old.
pub(super) fn get_build_status_xml(
    project: &str,
    pkg_name: &str,
    repo: &str,
    arch: &str,
    confirm: bool,
    fresh_since: Option<Instant>,
) -> Result<Option<BuildStatus>, Error> {
    if ![project, pkg_name, repo, arch]
        .iter()
        .all(|n| is_valid_obs_name(n))
    {
        return Ok(None);
    }
    let entries = match obs_package_entries(project, repo, arch, pkg_name, fresh_since) {
        Ok(Some(entries)) => entries,
        Ok(None) => return Ok(None),
        // An HTTP error (login, rate limit, outage) would repeat on the next
        // read, so it is reported rather than turned into an empty answer.
        Err(ApiFailure::Http(code, msg)) => {
            return Err(Error::OscFailed {
                cmd: format!("api /build/{project}/_result"),
                code: i32::from(code),
                stderr: msg,
            });
        }
        Err(ApiFailure::Other(msg)) => {
            crate::diag_failure!("  {msg}");
            return Ok(None);
        }
    };
    let Some(entry) = obs_status::entry_in(&entries, repo, arch, pkg_name) else {
        return Ok(None);
    };
    // The source revision is the package's; every flavor builds from it.
    let mut source: Option<Option<obs_status::SourceInfo>> = None;
    let mut resolve_entry = |entry: &obs_status::ResultEntry| {
        let evidence = if obs_status::needs_evidence(entry, confirm) {
            obs_status::Evidence {
                confirm,
                source: source
                    .get_or_insert_with(|| {
                        osc_api(&format!(
                            "/source/{}/{}?view=info",
                            enc(project),
                            enc(pkg_name)
                        ))
                        .and_then(|x| obs_status::parse_source_info(&x))
                    })
                    .clone(),
                job: osc_api(&format!(
                    "/build/{}/{}/{}/_jobhistory?package={}&limit=1",
                    enc(project),
                    enc(repo),
                    enc(arch),
                    enc(&entry.pkg)
                ))
                .and_then(|x| obs_status::parse_latest_job(&x)),
            }
        } else {
            obs_status::Evidence {
                confirm,
                ..Default::default()
            }
        };
        obs_status::resolve(entry, &evidence)
    };
    let base = resolve_entry(entry);
    let flavors: Vec<BuildStatus> = obs_status::flavors_in(&entries, repo, arch, pkg_name)
        .iter()
        .map(&mut resolve_entry)
        .collect();
    Ok(Some(obs_status::combine(base, &flavors)))
}
