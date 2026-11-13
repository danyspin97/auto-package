use super::*;

/// What a dry run says it would do with the prepared items `ready`, in place
/// of branching, building or submitting them. `branch_exists` is whether the
/// branch project is already on OBS.
pub(super) fn say_dry_run(
    a: &UpdateProjectArgs,
    branch: &str,
    branch_exists: bool,
    ready: &[Item],
    routes: &Routes,
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
            let phrase = tool_phrase(routes.tool(&i.pkg), routes.recap(&i.pkg));
            let how = if routes.mechanical(&i.pkg) {
                match &phrase {
                    None => {
                        "style fixes committed as they are (no build, no agent pass)".to_string()
                    }
                    Some(phrase) => {
                        format!(
                            "the tool {phrase} and commits them without a build (no agent pass)"
                        )
                    }
                }
            } else {
                let rebuilt = if a.no_agent {
                    "needs the agent (--no-agent is set), would be skipped"
                } else if i.findings.is_empty() {
                    "rebuilt by the pipeline, then the agent's review pass (no findings)"
                } else {
                    "rebuilt by the pipeline, the agent fixing the findings"
                };
                match &phrase {
                    None => rebuilt.to_string(),
                    Some(phrase) => format!("the tool {phrase} first; {rebuilt}"),
                }
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

/// What the tool does to a package's branch, for a dry run: the leftovers it
/// cleans and the upstream recap it fills in (`fill` is the versions it covers).
/// `None` when it does neither.
fn tool_phrase(tool: &[String], fill: Option<&str>) -> Option<String> {
    const NO_NOTES: &str = ap_build::request_review::ids::CHANGES_NO_UPSTREAM_NOTES;
    let cleans: Vec<&str> = tool
        .iter()
        .map(String::as_str)
        .filter(|n| *n != NO_NOTES)
        .collect();
    let mut parts = Vec::new();
    if !cleans.is_empty() {
        parts.push(format!("cleans the branch leftovers {}", cleans.join(", ")));
    }
    if fill.is_some() || tool.iter().any(|n| n == NO_NOTES) {
        parts.push(match fill {
            Some(versions) => format!("fills in the upstream recap for {versions}"),
            None => "fills in the upstream recap".to_string(),
        });
    }
    (!parts.is_empty()).then(|| parts.join(" and "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dry_run_names_the_recap_the_tool_fills_in() {
        let fill = ap_build::request_review::ids::CHANGES_NO_UPSTREAM_NOTES;
        assert_eq!(
            tool_phrase(&[fill.to_string()], Some("1.7.5..1.9.0")).as_deref(),
            Some("fills in the upstream recap for 1.7.5..1.9.0")
        );
        assert_eq!(
            tool_phrase(
                &["changes.dependency-bullet".to_string(), fill.to_string()],
                Some("1.7.5..1.9.0")
            )
            .as_deref(),
            Some(
                "cleans the branch leftovers changes.dependency-bullet and fills in the upstream recap for 1.7.5..1.9.0"
            )
        );
        assert_eq!(
            tool_phrase(&["FLAG".to_string()], None).as_deref(),
            Some("cleans the branch leftovers FLAG")
        );
        assert_eq!(tool_phrase(&[], None), None);
    }
}
