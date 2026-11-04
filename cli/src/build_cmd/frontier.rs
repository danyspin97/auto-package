use super::*;

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
                let limit = if rate_limited {
                    MAX_RATE_LIMITED_ROUNDS
                } else {
                    self.max_target_retries
                };
                if exhausted || *count >= limit {
                    self.permanently_failed.insert(rpm_name.clone());
                    self.give_ups.insert(
                        rpm_name.clone(),
                        if ladder {
                            format!("every resolution rung failed ({reason})")
                        } else if exhausted {
                            format!("it spent its full budget/attempt allowance ({reason})")
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
                        message: format!(
                            "{rpm_name} failed (attempt {count}/{limit}): \
                             {reason}; retrying next round"
                        ),
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
}
