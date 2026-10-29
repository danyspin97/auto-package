use super::*;

/// Settles a frontier round: stops when the target is built, or gives up
/// on the explicit target, and otherwise says whether the outer loop goes on.
pub(super) fn settle_round(b: Build<'_>, r: Round<'_>, s: Spawned) -> color_eyre::Result<bool> {
    let Spawned {
        spawned_any,
        batch_pending,
        batch_failures,
    } = s;
    let Build {
        pending_deps,
        attempted_deps,
        dep_drops,
        ..
    } = b;
    let Round {
        name,
        explicit_target,
        permanently_failed,
        give_ups,
        rx,
        in_flight,
        ..
    } = r;
    // Nothing was started this round: the closure is either built or
    // blocked, which is the terminal state checked here.
    if !spawned_any {
        if permanently_failed.is_empty() {
            ap_core::diag!("{name} and all dependencies are already built");
            return Ok(true);
        }
        for pkg in permanently_failed.iter() {
            give_ups
                .entry(pkg.clone())
                .or_insert_with(|| "no reason was recorded".to_string());
        }
        return Err(color_eyre::eyre::eyre!(build_incomplete_message(&give_ups)));
    }

    // Compute what, if anything, from `batch_pending` is actually new
    // information *before* merging it into `pending_deps` — the
    // explicit-target branch below needs to decide "did this round
    // discover any new missing deps, or are we spinning on the same ones
    // every pass" and the merged list hides that.
    let new_pending: Vec<String> = batch_pending
        .iter()
        .filter(|d| !attempted_deps.contains(&ap_build::osc::fuzzy_key(d)))
        .cloned()
        .collect();
    pending_deps.extend(batch_pending);

    // If an explicit target was requested, stop after one batch — there is
    // no "next round" to retry it in, so a failure here must surface as
    // an error rather than a silent `Ok`.
    if explicit_target.is_some() {
        if let Some((rpm_name, reason)) = batch_failures.first() {
            // Packages the batch left running are joined before returning.
            drain_in_flight(&rx, in_flight);
            return Err(color_eyre::eyre::eyre!("{rpm_name} failed: {reason}"));
        }
        // A `--target` run returning `MissingDeps` must not report success
        // for a target that was *not* built. Make unresolved work a
        // non-success outcome: if
        // this batch surfaced genuinely new deps (not ones we have
        // already tried and failed/gave-up on), loop around once to let
        // them build via the top-of-loop pending-deps processor and
        // re-select the target; if we would loop on the same deps
        // forever, error out instead.
        if !pending_deps.is_empty() && new_pending.is_empty() {
            let missing: Vec<&String> = pending_deps.iter().collect();
            let names = missing
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            drain_in_flight(&rx, in_flight);
            return Err(color_eyre::eyre::eyre!(
                "{name} needs {} but every one has already been attempted \
                 without success; cannot build. Why each one is out of reach:\n{}",
                names,
                dep_drops.explain(&pending_deps)
            ));
        }
        if pending_deps.is_empty() {
            return Ok(true);
        }
        // Fall through to the next loop iteration so `pending_deps` drains
        // (building each new dep), then `select_target` re-selects the
        // same explicit target and `run_package` retries with those deps
        // now available.
    }

    Ok(false)
}
