//! The project's packages that build from the same forge repository as a
//! package, for the held-back check (see `pins::held_back`).
//!
//! A sibling is found by its upstream, not its name (`influxdb2` for
//! `influxdb`, `zookeeper-kit` for `zookeeper`), so every package of the
//! project has to be read to know which ones are siblings. The index is built
//! once per plan, the first time a check asks for it, on `jobs` threads. Each
//! read goes through the plan's cache, so a package is asked of OBS once however
//! many checks look at it, and the package's own check reuses its read.
//!
//! A package whose read failed transiently (OBS did not answer, or the run was
//! interrupted) is not known to be a sibling or not. A check that would be held
//! by such a package cannot tell, so it is stopped rather than released.

use super::*;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// Reads the spec of a package, through the plan's cache.
pub(super) type SpecRead<'a> = dyn Fn(&str) -> Result<String, ReadError> + Sync + 'a;

/// A package of the project with a forge upstream: its name, upstream and
/// packaged version.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Sibling {
    name: String,
    upstream: String,
    version: String,
}

/// What one read of a package says for the index.
enum Probe {
    /// A package with a forge upstream.
    Sibling(Sibling),
    /// A package with no forge upstream, or whose spec is missing for good.
    Other,
    /// A read that failed transiently, or panicked: the reason.
    Unreadable(String),
}

/// The index: the siblings, and the packages that could not be read, each with
/// the reason, in name order.
#[derive(Default)]
struct Index {
    siblings: Vec<Sibling>,
    unreadable: Vec<(String, String)>,
}

/// The packages of a project, and the index of their forge upstreams that the
/// held-back check reads from.
pub struct ProjectSiblings<'a> {
    /// The packages, flavors (`boost:base`) aside, sorted.
    names: Vec<String>,
    jobs: usize,
    read: &'a SpecRead<'a>,
    /// Set when the plan stopped: nothing more is read.
    abandoned: AtomicBool,
    index: OnceLock<Index>,
}

fn no_spec(_: &str) -> Result<String, ReadError> {
    Err(ReadError::NoSpec)
}

/// The spec reader of a check with no project: it has no packages to read.
static NO_SPEC: fn(&str) -> Result<String, ReadError> = no_spec;

impl<'a> ProjectSiblings<'a> {
    /// The siblings of `names`, read with `read` on `jobs` threads.
    pub(super) fn new(
        names: impl IntoIterator<Item = String>,
        jobs: usize,
        read: &'a SpecRead<'a>,
    ) -> Self {
        let mut names: Vec<String> = names.into_iter().filter(|n| !n.contains(':')).collect();
        names.sort();
        names.dedup();
        Self {
            names,
            jobs,
            read,
            abandoned: AtomicBool::new(false),
            index: OnceLock::new(),
        }
    }

    /// A project with no other packages: no check reads a spec through it.
    pub(super) fn none() -> ProjectSiblings<'static> {
        ProjectSiblings::new([], 1, &NO_SPEC)
    }

    /// Stop reading: the plan has stopped and no more checks are kept.
    pub(super) fn abandon(&self) {
        self.abandoned.store(true, Ordering::Relaxed);
    }

    /// The other packages of the project on `upstream`, as `(name, packaged
    /// version)`, in name order. The first call reads the project's specs. Err
    /// when a package whose read failed might be one of them: the reason says
    /// which. A sibling whose spec is missing for good says nothing.
    pub(super) fn related(
        &self,
        rpm_name: &str,
        upstream: &str,
    ) -> Result<Vec<(String, String)>, String> {
        if self.names.is_empty() {
            return Ok(Vec::new());
        }
        if self.abandoned.load(Ordering::Relaxed) {
            return Err("the plan stopped before the sibling check".into());
        }
        let index = self.index.get_or_init(|| self.build());
        if let Some((name, why)) = index.unreadable.iter().find(|(n, _)| n != rpm_name) {
            return Err(format!(
                "the spec of {name} could not be read ({why}), so it is not known whether \
                 it is a sibling of this package"
            ));
        }
        Ok(index
            .siblings
            .iter()
            .filter(|s| s.name != rpm_name && s.upstream == upstream)
            .map(|s| (s.name.clone(), s.version.clone()))
            .collect())
    }

    /// The index of every package, whatever the order the threads read them in.
    /// A package whose read failed is read once more after the others, when the
    /// run is not stopping.
    fn build(&self) -> Index {
        let next = AtomicUsize::new(0);
        let found: Mutex<Vec<(usize, Probe)>> = Mutex::new(Vec::new());
        let read_one = || {
            if self.abandoned.load(Ordering::Relaxed) {
                return None;
            }
            let i = next.fetch_add(1, Ordering::Relaxed);
            let name = self.names.get(i)?;
            match probe(name, self.read) {
                Probe::Other => {}
                answer => found
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push((i, answer)),
            }
            Some(())
        };
        let read_one = &read_one;
        std::thread::scope(|scope| {
            for _ in 0..self.jobs.clamp(1, self.names.len()) {
                scope.spawn(move || while read_one().is_some() {});
            }
        });
        let mut found = found.into_inner().unwrap_or_else(|e| e.into_inner());
        found.sort_by_key(|(i, _)| *i);
        let mut index = Index::default();
        for (i, answer) in found {
            match answer {
                Probe::Sibling(s) => index.siblings.push(s),
                Probe::Unreadable(why) => {
                    let again =
                        if crate::proc::shutting_down() || self.abandoned.load(Ordering::Relaxed) {
                            Probe::Unreadable(why)
                        } else {
                            probe(&self.names[i], self.read)
                        };
                    match again {
                        Probe::Sibling(s) => index.siblings.push(s),
                        Probe::Other => {}
                        Probe::Unreadable(why) => {
                            index.unreadable.push((self.names[i].clone(), why))
                        }
                    }
                }
                Probe::Other => {}
            }
        }
        index
    }
}

/// Reads `name` and says what it is for the index. A read that panics is an
/// unreadable package, not a panic of the check.
fn probe(name: &str, read: &SpecRead<'_>) -> Probe {
    match catch_unwind(AssertUnwindSafe(|| read(name))) {
        Err(_) => Probe::Unreadable("the read panicked".into()),
        Ok(Err(ReadError::Failed(why))) => Probe::Unreadable(why),
        Ok(Err(_)) => Probe::Other,
        Ok(Ok(text)) => {
            let sibling = spec::parse_header(&text).and_then(|header| {
                let upstream = upstream_of(&header)?;
                Some(Sibling {
                    name: name.to_string(),
                    upstream,
                    version: header.version,
                })
            });
            sibling.map_or(Probe::Other, Probe::Sibling)
        }
    }
}

#[cfg(test)]
#[path = "siblings_tests.rs"]
mod tests;
