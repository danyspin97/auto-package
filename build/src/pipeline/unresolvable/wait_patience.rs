//! How long a package may keep waiting on a dependency. A wait record counts
//! the times it was consulted and the time it was made: a dependency whose
//! built commit never reaches the repository's copy would otherwise hold the
//! package forever, one Progress line and one loop turn per consultation. Past
//! either bound with no progress, the package needs a human (see
//! `wait_record::precheck_with`).

/// The consultations after which a wait is exhausted (the Nth consultation
/// is the one that exhausts it).
pub const MAX_CONSULTATIONS: u32 = 20;

/// The seconds after which a wait made with no progress is exhausted.
pub const MAX_WAIT_SECS: u64 = 6 * 60 * 60;

/// Why a wait is exhausted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exhausted {
    /// Consulted this many times.
    Consultations(u32),
    /// Made this many seconds ago.
    Age(u64),
}

/// Whether a wait consulted `consulted` times and made at `created` (unix
/// seconds; 0 for a record written before the field existed, whose age is not
/// known and is not counted) is exhausted at `now`.
pub fn exhausted(consulted: u32, created: u64, now: u64) -> Option<Exhausted> {
    if consulted >= MAX_CONSULTATIONS {
        return Some(Exhausted::Consultations(consulted));
    }
    let age = now.saturating_sub(created);
    (created != 0 && age >= MAX_WAIT_SECS).then_some(Exhausted::Age(age))
}

/// The counters `(consulted, created)` a wait made at `now` starts with.
/// `previous` is the record it replaces, with its counters. When `same` (the
/// same spec, RPMs and waited names: nothing progressed) the counters go on; a
/// record without a creation time reads as made `now`. Otherwise they start
/// again, which is the progress.
pub fn carried(previous: Option<(u32, u64)>, same: bool, now: u64) -> (u32, u64) {
    match previous {
        Some((consulted, created)) if same => (consulted, if created == 0 { now } else { created }),
        _ => (0, now),
    }
}

/// The Progress line of a wait whose dependency `dep` was built in this run
/// but whose commit is not in the repository's copy yet.
pub fn built_not_in_branch(dep: &str) -> String {
    format!("{dep} was built in this run but its commit is not in the branch copy yet")
}

/// The message of an exhausted wait on `dep`. `bound` is the version the spec
/// asks for, `branch` the version the repository's copy has (`None` when it
/// cannot be read), `built_here` whether `dep` was built in this run.
pub fn needs_human_message(
    dep: &str,
    why: Exhausted,
    bound: Option<&str>,
    branch: Option<&str>,
    built_here: bool,
) -> String {
    let waited = match why {
        Exhausted::Consultations(n) => format!("waited {n} times"),
        Exhausted::Age(secs) => format!("waited {} hours", secs / 3600),
    };
    let what = match (bound, branch) {
        (Some(b), Some(v)) => {
            format!("for {dep} >= {b} and the repository still has {v}")
        }
        (Some(b), None) => {
            format!("for {dep} >= {b}; the repository's copy of it cannot be read")
        }
        (None, _) => format!("for {dep}, which is not built here"),
    };
    let cause = if built_here && bound.is_some() {
        format!("; {}", built_not_in_branch(dep))
    } else {
        String::new()
    };
    format!(
        "needs a human: {waited} {what}{cause}. Next step: check the build of {dep} on OBS \
         and whether its commit reached the branch copy; if the bound is wrong, fix the \
         spec, otherwise build {dep} and retry"
    )
}
