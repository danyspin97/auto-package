use super::*;

/// What an independent `osc results` read (`table`) means for the status the
/// API document gave (`api`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrossCheck {
    /// The same answer: nothing to learn.
    Agrees,
    /// The API status is stale or unconfirmed and the table is not: the
    /// scheduler has taken up the new revision (`outdated`, `scheduled`,
    /// `blocked`, `building`), so the wait is progressing and polls briskly.
    NewRevisionSeen,
    /// The table's answer replaces the API's: the API document is stuck on
    /// a status the table has moved past.
    Adopt,
    /// A verdict from the table while the API says the newest sources are
    /// not built yet. The table cannot tell which revision a plain `failed`
    /// or `succeeded` belongs to, so it never settles the question.
    Ignore,
}

pub fn cross_check(api: &BuildStatus, table: &BuildStatus) -> CrossCheck {
    if api == table {
        CrossCheck::Agrees
    } else if awaiting_new_revision(api) {
        let scheduler_has_it = matches!(
            table,
            BuildStatus::Scheduled | BuildStatus::Blocked | BuildStatus::Building
        ) || awaiting_new_revision(table);
        if scheduler_has_it {
            CrossCheck::NewRevisionSeen
        } else {
            CrossCheck::Ignore
        }
    } else {
        CrossCheck::Adopt
    }
}

/// One status for a package and its multibuild flavors: still going while any
/// of them is (the first such), else the worst verdict — failed, broken, then
/// unresolvable — else succeeded if anything built, else the package's own.
pub fn combine(base: BuildStatus, flavors: &[BuildStatus]) -> BuildStatus {
    let all: Vec<&BuildStatus> = std::iter::once(&base).chain(flavors).collect();
    if let Some(active) = all.iter().find(|s| !s.is_terminal()) {
        return (*active).clone();
    }
    for bad in [
        BuildStatus::Failed,
        BuildStatus::Broken,
        BuildStatus::Unresolvable,
    ] {
        if all.contains(&&bad) {
            return bad;
        }
    }
    if all.iter().any(|s| s.is_success()) {
        return BuildStatus::Succeeded;
    }
    base
}

/// Everything beyond the `_result` document itself that [`resolve`] may need.
#[derive(Debug, Default, Clone)]
pub struct Evidence {
    pub source: Option<SourceInfo>,
    pub job: Option<JobInfo>,
    /// The caller has just committed sources and waits for their build: a
    /// built verdict (`succeeded`/`failed`) is trusted only when the job
    /// history confirms it, dirty flag or not. Right after a commit the
    /// repository is not yet flagged dirty, so the previous revision's
    /// verdict would otherwise read as the answer.
    pub confirm: bool,
}

/// Whether [`resolve`] needs [`Evidence`] for this entry — lets callers skip
/// two API calls in the common case. `confirm` is [`Evidence::confirm`].
pub fn needs_evidence(entry: &ResultEntry, confirm: bool) -> bool {
    verdict(entry).is_some_and(|v| {
        (entry.dirty && revision_dependent(&v)) || (confirm && is_built(&v, verdict_word(entry)))
    })
}

/// A verdict that comes from running a build (the job history records it),
/// as opposed to a scheduler judgement like `unresolvable`, `excluded` or
/// `unchanged` (no build was run: the sources produce what is already there).
pub(super) fn is_built(s: &BuildStatus, word: &str) -> bool {
    matches!(s, BuildStatus::Succeeded | BuildStatus::Failed) && word != "unchanged"
}

/// A package's verdict word: the code itself, or `<details>` for `finished`.
pub(super) fn verdict_word(entry: &ResultEntry) -> &str {
    if entry.code == "finished" {
        entry.details.as_deref().unwrap_or("finished")
    } else {
        &entry.code
    }
}

pub(super) fn verdict(entry: &ResultEntry) -> Option<BuildStatus> {
    let s = BuildStatus::from(verdict_word(entry));
    s.is_terminal().then_some(s)
}

/// Disabled/excluded come from project configuration, not from a source
/// revision, so a dirty repository cannot make them stale.
pub(super) fn revision_dependent(s: &BuildStatus) -> bool {
    !matches!(s, BuildStatus::Disabled | BuildStatus::Excluded)
}

pub(super) fn word_status(word: &str) -> BuildStatus {
    match word {
        // Transitional: the binary is still being published, so a dependent must
        // wait for it rather than schedule a second build of the same package.
        "dispatching" | "signing" | "publishing" | "unpublished" => BuildStatus::Building,
        // `finished` with the details `unchanged`: OBS found the new sources
        // build to the same result and kept the previous binaries.
        "unchanged" => BuildStatus::Succeeded,
        other => BuildStatus::from(other),
    }
}

/// Decide what one package's status means.
///
/// * Active states (`scheduled`, `building`, ...) map straight across.
/// * A bare `finished` is resolved through its `<details>` verdict.
/// * A terminal verdict in a `dirty` repository (or, with
///   [`Evidence::confirm`], any built verdict) may describe an older source
///   revision, so it is only trusted when the newest job in the job history
///   was built from the revision currently on the server — and then the
///   job's own result code wins (a non-verdict job code defers to the
///   `_result` one). Otherwise the result is `Unknown("stale: <word>")`, or
///   `Unknown("unconfirmed: <word>")` when no job information could be read;
///   callers keep waiting on both, the latter only for a bounded time.
pub fn resolve(entry: &ResultEntry, evidence: &Evidence) -> BuildStatus {
    let word = verdict_word(entry);
    let status = word_status(word);
    if !status.is_terminal() {
        return match (&status, entry.code.as_str()) {
            // `finished` with no readable verdict: not actionable yet.
            (BuildStatus::Unknown(_), "finished") => BuildStatus::Unknown("finished".to_string()),
            _ => status,
        };
    }
    // `broken: no source uploaded` is what the scheduler says about a package
    // it has not rescanned since the package was created, and the caller
    // waits on sources it has just committed: the verdict is old, whatever
    // the job history (a broken package never has a job) can or cannot say.
    // Accepting it as unconfirmed would fail a package whose build OBS starts
    // a minute later.
    if status == BuildStatus::Broken
        && (entry.dirty || evidence.confirm)
        && entry
            .details
            .as_deref()
            .is_some_and(|d| d.to_ascii_lowercase().contains("no source uploaded"))
    {
        return BuildStatus::Unknown(format!("stale: {word}"));
    }
    let must_confirm = (entry.dirty && revision_dependent(&status))
        || (evidence.confirm && is_built(&status, word));
    if !must_confirm {
        return status;
    }
    match (&evidence.source, &evidence.job) {
        (Some(src), Some(job)) if job.is_for(src) => {
            // `unchanged` is a success; a code that is no verdict at all
            // (`meta change`, ...) says nothing against the `_result` one,
            // which then stands for the current revision.
            let from_job = word_status(job.code.as_str());
            if from_job.is_terminal() {
                from_job
            } else {
                status
            }
        }
        // The newest job is from another revision: the verdict is old.
        (Some(_), Some(_)) => BuildStatus::Unknown(format!("stale: {word}")),
        // No job or source information could be read (empty or evicted job
        // history, a failed API call): nothing to confirm against, which
        // [`accept_unconfirmed`] lets a caller stop waiting for.
        _ => BuildStatus::Unknown(format!("{UNCONFIRMED}{word}")),
    }
}

pub(super) const UNCONFIRMED: &str = "unconfirmed: ";

/// The verdict behind an `Unknown("unconfirmed: <word>")` from [`resolve`]:
/// what to settle for once evidence has stayed unavailable for long enough.
pub fn accept_unconfirmed(s: &BuildStatus) -> Option<BuildStatus> {
    match s {
        BuildStatus::Unknown(t) => t.strip_prefix(UNCONFIRMED).map(word_status),
        _ => None,
    }
}

/// Resolve ignoring staleness: the dirty flag is dropped, so every terminal
/// verdict is taken at face value. For project-wide scans that only decide
/// what is worth retrying, where evidence per package would cost two API
/// calls each.
pub fn resolve_lenient(entry: &ResultEntry) -> BuildStatus {
    let mut e = entry.clone();
    e.dirty = false;
    resolve(&e, &Evidence::default())
}
