use super::*;

/// What a dry run says it would do with the prepared items `ready`, in place
/// of branching, building or submitting them. `branch_exists` is whether the
/// branch project is already on OBS.
pub(super) fn say_dry_run(
    a: &UpdateProjectArgs,
    branch: &str,
    branch_exists: bool,
    ready: &[Item],
    mechanical: &HashSet<String>,
    ctx: &Ctx<'_>,
    report: &mut Report,
) {
    if let Some(job) = &a.backport_job {
        ctx.say(format!(
            "dry run: nothing is branched, built or submitted; {} stays untouched",
            a.source
        ));
        for i in ready {
            for line in ap_build::backport::dry_run_lines(job, &i.pkg, &i.new_version, branch) {
                ctx.say(line);
            }
        }
        report.paths.clear();
        return;
    }
    if let Some(ap_build::bugfix::FixTask::Bug(job)) = a.fix_task.as_deref() {
        ctx.say(format!(
            "dry run: nothing is branched, built, committed or submitted; {} stays untouched",
            a.source
        ));
        for i in ready {
            for line in
                ap_build::bugfix::dry_run_lines(job, &i.new_version, branch, &a.source, a.submit)
            {
                ctx.say(line);
            }
        }
        report.paths.clear();
        return;
    }
    if a.improve {
        ctx.say(format!(
            "dry run: nothing is branched, built, edited or submitted; {} stays untouched",
            a.source
        ));
        for i in ready {
            let how = if mechanical.contains(&i.pkg) {
                "style fixes committed as they are"
            } else if a.no_agent {
                "needs the agent (--no-agent is set), would be skipped"
            } else {
                "rebuilt by the pipeline, the agent fixing the findings"
            };
            ctx.say(format!("  {}: {how}", i.pkg));
        }
        if a.in_place {
            ctx.say(format!(
                "the fixes would be committed into {} itself",
                a.source
            ));
        } else if a.submit {
            ctx.say(format!(
                "each package that builds in {branch} would get a submit request to {}",
                a.source
            ));
        } else {
            ctx.say(format!(
                "they would be branched into {branch}; submit requests: none (--submit not given)"
            ));
        }
        report.paths.clear();
        return;
    }
    if let Some(r) = &a.revision {
        ctx.say(format!(
            "revision {r} would be branched, and {branch} must not have the package yet"
        ));
    }
    let planned: Vec<PathAddition> = report.paths.clone();
    for line in up::dry_run_lines(&up::DryRun {
        source: &a.source,
        branch_project: branch,
        branch_exists,
        items: ready,
        paths: &planned,
        maintainer: &report.maintainer,
        submit: a.submit,
    }) {
        ctx.say(line);
    }
    for line in up::requested_lines(&report.requested) {
        ctx.say(line);
    }
    report.paths.clear();
}
