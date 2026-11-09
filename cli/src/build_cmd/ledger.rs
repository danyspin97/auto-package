use super::*;
use ap_build::pipeline::{RepeatStep, RetryRecord};

/// What a round of dependency builds leaves for the rest of the run.
pub(super) struct DepLedger<'a> {
    pub(super) attempted_deps: &'a mut std::collections::HashSet<String>,
    pub(super) dep_rounds: &'a mut std::collections::HashMap<String, usize>,
    pub(super) retrying: &'a mut std::collections::HashSet<String>,
    pub(super) pending_deps: &'a mut Vec<String>,
    pub(super) retry_group: &'a mut Vec<String>,
    pub(super) config: &'a mut PipelineConfig,
    pub(super) patch_requests: &'a PatchRequests,
    pub(super) native_names: &'a NativeNames,
    pub(super) outdated_names: &'a OutdatedNames,
    pub(super) dep_drops: &'a DepDrops,
    pub(super) pkg_reporter: &'a dyn Fn(&str) -> Arc<dyn Reporter + Send + Sync>,
    pub(super) max_dep_attempts: usize,
    pub(super) repeats: &'a mut RepeatMemory,
    pub(super) state_root: &'a std::path::Path,
}

pub(super) type DepHandle = (
    String,
    String,
    std::thread::JoinHandle<PipelineResult>,
    String,
);

impl DepLedger<'_> {
    /// Record how each dependency build in `handles` ended.
    pub(super) fn record(&mut self, handles: Vec<DepHandle>) {
        for (dep_rpm, version, handle, dep) in handles {
            let result = join_package(handle, &dep_rpm);
            // Aborted run: the results are not outcomes of the packages.
            if ap_build::proc::abort_reason().is_some() {
                continue;
            }
            let key = ap_build::osc::fuzzy_key(&dep);
            match result {
                PipelineResult::Built { .. } => {
                    ap_core::diag!("built dep {dep_rpm}");
                    self.repeats.builds += 1;
                    self.repeats.last.remove(&dep_rpm);
                    ap_build::pipeline::clear_record(self.state_root, &dep_rpm);
                    self.dep_drops.note_outcome(
                        &dep_rpm,
                        "it was built this run, yet the package that needs it still \
                         reports it missing (a name mismatch?)",
                    );
                }
                PipelineResult::AlreadyBuilt { .. } => {
                    ap_core::diag!("dep {dep_rpm} already built");
                    self.dep_drops.note_outcome(
                        &dep_rpm,
                        "it was already built, yet the package that needs it still \
                         reports it missing (a name mismatch?)",
                    );
                }
                PipelineResult::MissingDeps {
                    deps: more,
                    patches,
                    native,
                    outdated,
                    ..
                } => {
                    self.patch_requests.record(&patches);
                    self.native_names.record(&native);
                    self.outdated_names.record(&outdated);
                    self.dep_drops.note_outcome(
                        &dep_rpm,
                        format!("it could not be built until {} exist", more.join(", ")),
                    );
                    // It needs packages of its own: once those have had
                    // their batch it is run again, instead of staying
                    // "attempted" while the package waiting on it is
                    // abandoned. Bounded like any other retry.
                    let rounds = self.dep_rounds.entry(key.clone()).or_insert(0);
                    *rounds += 1;
                    if !more.is_empty() && *rounds < self.max_dep_attempts {
                        self.attempted_deps.remove(&key);
                        self.retrying.insert(key);
                        self.retry_group.push(dep);
                    }
                    self.pending_deps.extend(more);
                }
                PipelineResult::Failed { reason, kind, .. } => {
                    // A dependency OBS or the pipeline failed is run
                    // again through the normal pipeline when the package
                    // needing it names it once more, rather than counted
                    // as attempted and dropped after one go — unless it
                    // used up its allowance, which a second round would
                    // only repeat. Rounds are bounded and an attempt
                    // within a round resumes the package's persisted
                    // attempt count.
                    let rounds = self.dep_rounds.entry(key.clone()).or_insert(0);
                    *rounds += 1;
                    // The same ordinary failure with no build since is not
                    // run again: it would repeat the same failure.
                    let repeated = identical_stop_applies(kind)
                        && repeats_without_change(
                            self.repeats.last.get(&dep_rpm),
                            &reason,
                            self.repeats.builds,
                        );
                    self.repeats.last.insert(
                        dep_rpm.clone(),
                        LastFailure {
                            reason: reason.clone(),
                            builds: self.repeats.builds,
                        },
                    );
                    let (step, record) = if identical_stop_applies(kind) {
                        ap_build::pipeline::record_failure(
                            self.state_root,
                            &dep_rpm,
                            &version,
                            &reason,
                            repeated,
                        )
                    } else {
                        (RepeatStep::Retry, RetryRecord::default())
                    };
                    let stop = step == RepeatStep::Stop;
                    let retry = dep_failure_retries(kind, *rounds, self.max_dep_attempts) && !stop;
                    if retry {
                        self.attempted_deps.remove(&key);
                        self.retrying.insert(key);
                        // Run again once the rest of this batch has had its
                        // turn, whether or not a dependent names it again:
                        // the dependent may have given up on seeing it failed.
                        self.retry_group.push(dep.clone());
                    }
                    self.dep_drops.note_outcome(
                        &dep_rpm,
                        if stop {
                            record.give_up_text()
                        } else {
                            format!(
                                "its build failed ({reason}){}",
                                if retry {
                                    "; it is tried once more if it is still needed"
                                } else {
                                    ""
                                }
                            )
                        },
                    );
                    (self.pkg_reporter)(&dep_rpm).report(&Event::Warning {
                        step: None,
                        message: if step == RepeatStep::Instruct {
                            format!(
                                "dependency {dep_rpm} failed again with the same reason \
                                 ({reason}); its next attempt is told to try a different \
                                 approach"
                            )
                        } else {
                            format!(
                                "dependency {dep_rpm} failed to build ({reason}); \
                                 self.retrying the package that needs it"
                            )
                        },
                    });
                    // Record it so the dependent package's agent can see
                    // that this one was attempted — without that evidence
                    // it can only report the dep again and stall.
                    if !self.config.failed_deps.contains(&dep_rpm) {
                        self.config.failed_deps.push(dep_rpm);
                    }
                }
            }
        }
    }
}
