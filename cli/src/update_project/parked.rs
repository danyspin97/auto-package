//! The packages `--update-deps` puts aside while they wait for a same-project
//! update the sweep has not found yet (see [`super::same_project_defer`]).
//!
//! A parked package keeps what was read for it, so resuming it reads nothing
//! again. [`release_first`] walks the deferred packages in the order the
//! wait graph gives (each after those it waits for) and takes the first one
//! that `judge` says no longer waits. Once the sweep has finished, every
//! parked package is judged so, and the releases go on one at a time, each
//! after the packages it waits for.

use ap_build::osc::fuzzy_key;

/// The parked packages by [`fuzzy_key`] of their name, in the order they were
/// parked.
pub(super) struct Parked<T> {
    entries: Vec<(String, T)>,
}

impl<T> Default for Parked<T> {
    fn default() -> Self {
        Parked {
            entries: Vec::new(),
        }
    }
}

impl<T> Parked<T> {
    pub(super) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Park `value` for `pkg`. A package parked again replaces its value and
    /// keeps its place.
    pub(super) fn park(&mut self, pkg: &str, value: T) {
        let key = fuzzy_key(pkg);
        match self.entries.iter_mut().find(|(k, _)| *k == key) {
            Some(entry) => entry.1 = value,
            None => self.entries.push((key, value)),
        }
    }

    pub(super) fn get_mut(&mut self, pkg: &str) -> Option<&mut T> {
        let key = fuzzy_key(pkg);
        self.entries
            .iter_mut()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v)
    }

    /// Take `pkg` out of the parked packages, its value with it.
    pub(super) fn take(&mut self, pkg: &str) -> Option<T> {
        let key = fuzzy_key(pkg);
        let at = self.entries.iter().position(|(k, _)| *k == key)?;
        Some(self.entries.remove(at).1)
    }

    /// Every parked value, in the order they were parked: what a run that
    /// stops before releasing them lists as not started.
    pub(super) fn into_values(self) -> Vec<T> {
        self.entries.into_iter().map(|(_, v)| v).collect()
    }
}

/// Release the first package of `order` (keys as the wait graph has them)
/// that is parked and that `judge` releases: `judge` gets the key and the
/// parked value, and returns what the release is told with, or `None` while
/// the package still waits. The package is taken out of `parked`, and returned
/// with its key and what `judge` returned. `None` when none is released.
pub(super) fn release_first<T, R>(
    parked: &mut Parked<T>,
    order: &[String],
    mut judge: impl FnMut(&str, &mut T) -> Option<R>,
) -> Option<(String, T, R)> {
    for key in order {
        let Some(value) = parked.get_mut(key) else {
            continue;
        };
        if let Some(released) = judge(key, value) {
            let value = parked.take(key)?;
            return Some((key.clone(), value, released));
        }
    }
    None
}
