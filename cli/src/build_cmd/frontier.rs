use super::*;
use ap_build::pipeline::{RepeatStep, RetryRecord};

/// Where the packages of a frontier round leave their results: the retry
/// counts, the give-ups and what the round still has to do.
pub(super) struct Frontier<'a> {
    pub(super) fail_counts: &'a mut std::collections::HashMap<String, usize>,
    pub(super) rate_limited_counts: &'a mut std::collections::HashMap<String, usize>,
    pub(super) give_ups: &'a mut std::collections::BTreeMap<String, String>,
    pub(super) permanently_failed: &'a mut std::collections::HashSet<String>,
    pub(super) attempted_deps: &'a std::collections::HashSet<String>,
    pub(super) batch_pending: &'a mut Vec<String>,
    pub(super) batch_failures: &'a mut Vec<(String, String)>,
    pub(super) patch_requests: &'a PatchRequests,
    pub(super) native_names: &'a NativeNames,
    pub(super) outdated_names: &'a OutdatedNames,
    pub(super) dep_drops: &'a DepDrops,
    pub(super) state_root: &'a Path,
    pub(super) pkg_reporter: &'a dyn Fn(&str) -> Arc<dyn Reporter + Send + Sync>,
    pub(super) max_target_retries: usize,
    pub(super) repeats: &'a mut RepeatMemory,
}

/// What a package last failed with, and how many builds of the run had
/// finished by then. A failure with the same reason and no build since has
/// no new input that could change its outcome.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct LastFailure {
    pub(super) reason: String,
    pub(super) builds: usize,
}

/// Per run: the last failure of each package, and how many packages have
/// been built so far (a build may be a dependency a failure was waiting on).
#[derive(Default)]
pub(super) struct RepeatMemory {
    pub(super) builds: usize,
    pub(super) last: std::collections::HashMap<String, LastFailure>,
}

/// Whether a failure of this kind takes the repeat path. An ordinary failure
/// (`Other`) can repeat identically: its repeat is instructed to try something
/// different, and only the instructed attempts failing again stop it (see
/// `RetryRecord`). Transient, disk, quota, busy and agent-side failures are
/// retried up to the cap, since their next attempt may well differ.
pub(super) fn identical_stop_applies(kind: FailureKind) -> bool {
    kind == FailureKind::Other
}

/// Whether a failure with `reason` repeats the previous failure of the same
/// package with no build finished in between: a retry would start from the
/// same inputs and fail the same way. The reasons are compared with what changes
/// between attempts left out (a line number, a temporary path, a commit message).
pub(super) fn repeats_without_change(
    prev: Option<&LastFailure>,
    reason: &str,
    builds: usize,
) -> bool {
    prev.is_some_and(|p| ap_build::pipeline::same_failure(&p.reason, reason) && p.builds == builds)
}

/// Whether a package that has failed `count` times has used up its `limit`.
pub(super) fn cap_reached(count: usize, limit: usize) -> bool {
    count >= limit
}

impl Frontier<'_> {
    /// Record how the package `rpm_name` finished in this round.
    /// `false` when the package was not tried here (another process holds it),
    /// so the round does not go on to the pending check.
    pub(super) fn settle(
        &mut self,
        rpm_name: &String,
        version: &String,
        result: PipelineResult,
    ) -> bool {
        const MAX_RATE_LIMITED_ROUNDS: usize = 200;
        match result {
            PipelineResult::Built { .. } => {
                ap_core::diag!("built {rpm_name} {version}");
                self.fail_counts.remove(rpm_name);
                self.repeats.builds += 1;
                self.repeats.last.remove(rpm_name);
                ap_build::pipeline::clear_record(self.state_root, rpm_name);
                ap_build::pipeline::note_built(rpm_name);
            }
            PipelineResult::AlreadyBuilt { .. } => {
                ap_core::diag!("{rpm_name} {version} already built");
                self.fail_counts.remove(rpm_name);
            }
            PipelineResult::MissingDeps {
                deps,
                patches,
                native,
                outdated,
                ..
            } => {
                self.patch_requests.record(&patches);
                self.native_names.record(&native);
                self.outdated_names.record(&outdated);
                // No-progress detection. A package that keeps
                // surfacing MissingDeps on names already in
                // `self.attempted_deps` — and therefore skipped by the
                // mini-loop every time — is not making progress; it is
                // blocked on something that can never be built in this
                // environment (closed-source toolchain, PyPI name that
                // doesn't exist, …). Without this, the frontier would
                // re-select the same package forever: MissingDeps does
                // not consume the outer failed-target limit, so the
                // loop never gives up on it.
                //
                // Treat such a no-progress round the same as a Failed
                // result for retry-counting purposes: increment
                // `self.fail_counts`, and after `self.max_target_retries` of
                // genuinely-no-new-deps rounds, mark the package
                // self.permanently_failed and let the frontier move on.
                //
                // A dep attempted earlier may have been built locally since
                // (by a sibling in this round, say); it is available now, so
                // it counts as progress, not as an exhausted dependency.
                let remaining: Vec<&String> = deps
                    .iter()
                    .filter(|d| {
                        built_locally(&self.state_root, &ap_build::osc::rpm_name(d)).is_none()
                    })
                    .collect();
                let new_deps: Vec<&&String> = remaining
                    .iter()
                    .filter(|d| !self.attempted_deps.contains(&ap_build::osc::fuzzy_key(d)))
                    .collect();
                if new_deps.is_empty() && !remaining.is_empty() {
                    // Repeating this package cannot help: every dependency it
                    // names was already attempted and dropped by the dep queue,
                    // and attempted names are never queued again. Give up on the
                    // first such round instead of re-running the pipeline up to
                    // `self.max_target_retries` times at real agent cost.
                    let count = self.fail_counts.entry(rpm_name.clone()).or_insert(0);
                    *count += 1;
                    self.permanently_failed.insert(rpm_name.clone());
                    self.give_ups.insert(
                        rpm_name.clone(),
                        format!(
                            "it needs {} and none of them can be queued again:\n{}",
                            deps.join(", "),
                            self.dep_drops.explain(&deps)
                        ),
                    );
                    (self.pkg_reporter)(&rpm_name).report(&Event::Warning {
                        step: None,
                        message: format!(
                            "giving up on {rpm_name}: it needs {} and each of those was \
                             already attempted and dropped; anything depending on it \
                             stays blocked. Why each one is out of reach:\n{}",
                            deps.join(", "),
                            self.dep_drops.explain(&deps)
                        ),
                    });
                }
                self.batch_pending.extend(deps);
            }
            PipelineResult::Failed { reason, kind, .. } => {
                self.batch_failures.push((rpm_name.clone(), reason.clone()));
                if kind == FailureKind::Busy {
                    // Another process is building this package, so nothing
                    // was tried here. It is deferred to a later round and
                    // does not count toward giving up.
                    (self.pkg_reporter)(&rpm_name).report(&Event::Warning {
                        step: None,
                        message: format!(
                            "{rpm_name} is being built by another process ({reason}); \
                             deferred to a later round"
                        ),
                    });
                    return false;
                }
                if kind == FailureKind::NeedsMaintainer {
                    // A decision for a maintainer: another round would only
                    // repeat the same refusal.
                    self.permanently_failed.insert(rpm_name.clone());
                    self.give_ups.insert(
                        rpm_name.clone(),
                        format!("it needs a maintainer ({reason})"),
                    );
                    (self.pkg_reporter)(&rpm_name).report(&Event::Warning {
                        step: None,
                        message: format!("{rpm_name} needs a maintainer: {reason}"),
                    });
                    return true;
                }
                if settles_as_skip(kind) {
                    // OBS does not build the package for this repository and
                    // arch: another round gets the same answer, so it is given
                    // up at once, as a skip, and not counted as a failed attempt.
                    self.permanently_failed.insert(rpm_name.clone());
                    self.give_ups.insert(
                        rpm_name.clone(),
                        format!("OBS does not build it here ({reason})"),
                    );
                    (self.pkg_reporter)(&rpm_name).report(&Event::Warning {
                        step: None,
                        message: format!("{rpm_name} is skipped: {reason}"),
                    });
                    return true;
                }
                let rate_limited = kind == FailureKind::RateLimited;
                // A package that already spent its *entire* budget or attempt
                // allowance on this round gives up at once, instead of
                // getting a fresh budget for each of `self.max_target_retries`
                // rounds. `self.max_target_retries` reuses `max_attempts` as the
                // whole-round retry count, so without this a package that
                // burned its budget would repeat the same loop over and over.
                let exhausted = kind == FailureKind::Exhausted;
                // A package whose resolution ladder was spent is not run
                // again: the same spec would fail the same ladder again. It
                // is a failure of the tool to investigate, never a `blocked`.
                let ladder = reason.starts_with(ap_build::pipeline::LADDER_FAILED_PREFIX);
                let count = if rate_limited {
                    self.rate_limited_counts
                        .entry(rpm_name.clone())
                        .or_insert(0)
                } else {
                    self.fail_counts.entry(rpm_name.clone()).or_insert(0)
                };
                *count += 1;
                // The same ordinary failure, with no build since the last
                // one, is not run again as it was: the next attempt is told
                // what already failed and to try something different. Only
                // the instructed attempts failing the same way again stop it.
                let repeated = identical_stop_applies(kind)
                    && repeats_without_change(
                        self.repeats.last.get(rpm_name),
                        &reason,
                        self.repeats.builds,
                    );
                self.repeats.last.insert(
                    rpm_name.clone(),
                    LastFailure {
                        reason: reason.clone(),
                        builds: self.repeats.builds,
                    },
                );
                let (step, record) = if identical_stop_applies(kind) {
                    ap_build::pipeline::record_failure(
                        self.state_root,
                        rpm_name,
                        version,
                        &reason,
                        repeated,
                    )
                } else {
                    (RepeatStep::Retry, RetryRecord::default())
                };
                let stop = step == RepeatStep::Stop;
                let limit = if rate_limited {
                    MAX_RATE_LIMITED_ROUNDS
                } else {
                    self.max_target_retries
                };
                if exhausted || stop || cap_reached(*count, limit) {
                    self.permanently_failed.insert(rpm_name.clone());
                    self.give_ups.insert(
                        rpm_name.clone(),
                        if ladder {
                            format!("every resolution rung failed ({reason})")
                        } else if exhausted {
                            format!("it spent its full budget/attempt allowance ({reason})")
                        } else if stop {
                            record.give_up_text()
                        } else {
                            format!("it failed {count} times in a row (last: {reason})")
                        },
                    );
                    (self.pkg_reporter)(&rpm_name).report(&Event::Warning {
                        step: None,
                        message: if ladder {
                            format!(
                                "{rpm_name} is unresolved: every rung of the resolution \
                                 ladder failed ({reason}); this is a defect of the tool to \
                                 investigate. Anything depending on it stays waiting, the \
                                 rest of the closure keeps going"
                            )
                        } else if exhausted {
                            format!(
                                "giving up on {rpm_name}: it already spent its full \
                                 budget/attempt allowance this round without succeeding \
                                 ({reason}) — another round would just repeat the same \
                                 fix loop with a fresh budget; anything depending on it \
                                 stays blocked, but the rest of the closure keeps going"
                            )
                        } else if stop {
                            format!(
                                "giving up on {rpm_name}: {}. Anything depending on it \
                                 stays blocked, but the rest of the closure keeps going",
                                record.give_up_text()
                            )
                        } else {
                            format!(
                                "giving up on {rpm_name} after {count} failed attempts \
                                 ({reason}); anything depending on it stays blocked, but \
                                 the rest of the closure keeps going"
                            )
                        },
                    });
                } else {
                    (self.pkg_reporter)(&rpm_name).report(&Event::Warning {
                        step: None,
                        message: if step == RepeatStep::Instruct {
                            format!(
                                "{rpm_name} failed again with the same reason ({reason}); \
                                 its next attempt is told to try a different approach"
                            )
                        } else {
                            format!(
                                "{rpm_name} failed (attempt {count}/{limit}): \
                                 {reason}; retrying next round"
                            )
                        },
                    });
                }
            }
        }
        true
    }
}

/// Whether a failure settles the package as a skip at once: OBS does not build
/// it for this repository and arch (excluded or disabled there), so it is not
/// retried and not counted toward giving up.
pub(super) fn settles_as_skip(kind: FailureKind) -> bool {
    kind == FailureKind::Excluded
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_excluded_build_settles_as_a_skip_at_once() {
        assert!(settles_as_skip(FailureKind::Excluded));
        assert!(!settles_as_skip(FailureKind::Other));
        assert!(!settles_as_skip(FailureKind::NeedsMaintainer));
    }

    fn last(reason: &str, builds: usize) -> LastFailure {
        LastFailure {
            reason: reason.to_string(),
            builds,
        }
    }

    #[test]
    fn the_same_reason_twice_with_nothing_built_between_is_a_repeat() {
        let prev = last("needs a human: 1 patch", 4);
        assert!(repeats_without_change(
            Some(&prev),
            "needs a human: 1 patch",
            4
        ));
    }

    #[test]
    fn a_reason_that_differs_only_in_a_line_or_a_temporary_name_is_a_repeat() {
        let prev = last(
            "osc ci: python-chess.changes:23 INVALID DATE /tmp/.tmpAb12Cd/x",
            4,
        );
        assert!(repeats_without_change(
            Some(&prev),
            "osc ci: python-chess.changes:25 INVALID DATE /tmp/.tmpZz98Yy/x",
            4
        ));
    }

    #[test]
    fn a_first_failure_is_never_a_repeat() {
        assert!(!repeats_without_change(None, "needs a human", 0));
    }

    #[test]
    fn a_changed_reason_is_retried() {
        let prev = last("needs a human: 1 patch", 4);
        assert!(!repeats_without_change(
            Some(&prev),
            "needs a human: 2 patches",
            4
        ));
    }

    #[test]
    fn a_dependency_built_in_between_is_retried() {
        let prev = last("not started: OBS baseline", 4);
        assert!(!repeats_without_change(
            Some(&prev),
            "not started: OBS baseline",
            5
        ));
    }

    #[test]
    fn only_ordinary_failures_use_the_identical_stop() {
        assert!(identical_stop_applies(FailureKind::Other));
        for kind in [
            FailureKind::Transient,
            FailureKind::DiskFull,
            FailureKind::RateLimited,
            FailureKind::Busy,
            FailureKind::AgentFailed,
            FailureKind::ConditionUnmet,
        ] {
            assert!(!identical_stop_applies(kind), "{kind:?} must be retried");
        }
    }

    #[test]
    fn a_transient_failure_is_retried_up_to_the_cap() {
        // Transient failures never take the identical stop, so only the cap
        // decides: retry while under the limit, give up once it is reached.
        assert!(!identical_stop_applies(FailureKind::Transient));
        assert!(!cap_reached(1, 3));
        assert!(!cap_reached(2, 3));
        assert!(cap_reached(3, 3));
    }
}
