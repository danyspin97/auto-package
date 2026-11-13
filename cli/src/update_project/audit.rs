use super::*;

/// The audit of `lint`: what is wrong with each package of the source, printed
/// as a report once it is done. It runs on its own thread, and every package
/// with findings that the pipeline can rebuild arrives on `items` as soon as it
/// is audited, with whether its findings are all style slips, which are
/// committed without a build.
pub(super) struct Audit {
    pub(super) items: std::sync::mpsc::Receiver<(Item, bool)>,
    handle: std::thread::JoinHandle<Result<AuditEnd, String>>,
}

/// How an audit ended.
pub(super) struct AuditEnd {
    /// The packages audited.
    pub(super) packages: usize,
    /// The packages handed on to be improved.
    pub(super) items: usize,
    /// The packages of the source the run was asked for (`--only`), audited,
    /// skipped or unreadable alike: none means nothing matched.
    pub(super) candidates: usize,
    /// How many of the candidates OBS answered 404 for (absent from OBS).
    pub(super) absent: usize,
    /// The rpm names of every candidate, for the names asked for that matched none.
    pub(super) matched: Vec<String>,
    /// The packages whose spec could not be read, each with the reason: OBS
    /// failed the read, so these were not checked.
    pub(super) unreadable: Vec<String>,
}

impl Audit {
    /// `branch` is the branch project the run builds in: the rpmlintrc files
    /// are read from it when `lint` continues from it.
    pub(super) fn start(
        a: &UpdateProjectArgs,
        branch: &str,
        reporter: Arc<dyn Reporter + Send + Sync>,
    ) -> Self {
        let (tx, items) = std::sync::mpsc::channel();
        let (source, distro, arch) = (a.source.clone(), a.distro.clone(), a.arch.clone());
        let (only, exclude) = (a.only.clone(), a.exclude.clone());
        let agent = !a.no_agent;
        let (branch, continues, in_place) = (
            branch.to_string(),
            super::existing_lint::applies(a),
            a.in_place,
        );
        let handle = std::thread::spawn(move || {
            let say = |message: String| {
                reporter.report(&Event::Progress {
                    step: None,
                    message,
                })
            };
            let warn = |message: String| {
                reporter.report(&Event::Warning {
                    step: None,
                    message,
                })
            };
            let emit = Emit::run(reporter.as_ref());
            let distro = osc::choose_repository(&source, distro.as_deref(), &arch).0;
            let statuses = osc::get_project_build_statuses(&source, &distro, &arch)
                .map_err(|e| {
                    warn(format!(
                        "could not read the build results of {source}, so broken builds and rpmlint are not checked: {e}"
                    ))
                })
                .unwrap_or_default();

            let mut audits: Vec<quality::PackageAudit> = Vec::new();
            let mut sent = 0;
            let mut seen: HashSet<String> = HashSet::new();
            let ldconfig = ap_build::spec_tidy::hygiene::ldconfig_macro_defined(&distro);
            let hosts = ap_build::allowed_hosts::load_default();
            let mut ldconfig_left = false;
            let mut unreadable: Vec<String> = Vec::new();
            let mut absent = 0usize;
            let mut matched: Vec<String> = Vec::new();
            update::plan_each(
                &source,
                &|n| wanted_by(&only, &exclude, n),
                emit,
                &mut |c| {
                    matched.push(c.rpm_name.clone());
                    let pkg = up::branch_name(&c.rpm_name);
                    if !seen.insert(pkg.to_string()) {
                        return true;
                    }
                    let spec = match update::planned_spec(&source, pkg, emit) {
                        Ok(spec) => spec,
                        Err(e) => {
                            // The reason alone: a failed read's Display already says
                            // "the spec could not be read", which the warning says too.
                            let why = match &e {
                                update::ReadError::Failed(why) => why.clone(),
                                other => other.to_string(),
                            };
                            warn(format!(
                                "could not read the spec of {pkg}, so it is not checked: {why}"
                            ));
                            // A failed read is not a package that is not there (a 404,
                            // or no spec at all): the run is incomplete.
                            if let update::ReadError::Failed(why) = &e {
                                unreadable.push(format!("{pkg}: {why}"));
                            }
                            if matches!(e, update::ReadError::Absent(_)) {
                                absent += 1;
                            }
                            return true;
                        }
                    };
                    ldconfig_left |= ap_build::spec_tidy::hygiene::ldconfig_left(&spec, ldconfig);
                    let mut findings = quality::audit_spec(&spec, ldconfig);
                    let ecosystem_of = match &c.decision {
                        update::Decision::Update { ecosystem, .. }
                        | update::Decision::UpToDate { ecosystem, .. } => Some(ecosystem),
                        update::Decision::Skipped(_) => None,
                    };
                    findings.extend(super::provenance::provenance_findings(
                        &source,
                        pkg,
                        &spec,
                        emit,
                        &|m| warn(m),
                        &hosts,
                    ));
                    findings.extend(super::rpmlint_audit::findings(
                        &source,
                        Some(branch.as_str()),
                        continues,
                        pkg,
                        ecosystem_of,
                        &spec,
                        emit,
                        &|m| warn(m),
                    ));
                    let status = statuses.get(&c.rpm_name).or_else(|| statuses.get(pkg));
                    if let Some(status) = status {
                        findings.extend(quality::audit_status(status));
                        if status.is_success()
                            && let Some(log) =
                                osc::fetch_build_log_tail(&source, pkg, &distro, &arch)
                        {
                            findings.extend(quality::rpmlint_findings(&log));
                        }
                    }
                    findings.sort_by_key(|f| f.kind);

                    let (ecosystem, version) = match &c.decision {
                        update::Decision::Update {
                            ecosystem,
                            old_version,
                            ..
                        } => (Some(ecosystem.clone()), old_version.clone()),
                        update::Decision::UpToDate { ecosystem, version } => {
                            (Some(ecosystem.clone()), version.clone())
                        }
                        update::Decision::Skipped(_) => (None, String::new()),
                    };
                    // A package with unpublished work on its branch is checked against
                    // its origin first: its branch leftovers are the tool's to clean.
                    let carrying = super::leftovers::carry(
                        &source,
                        &branch,
                        pkg,
                        in_place,
                        continues,
                        ecosystem_of,
                        emit,
                    );
                    let gate = match carrying {
                        Some(carry) => {
                            let obs = ap_build::request_review::fetch::LiveRequestObs { emit };
                            super::leftovers::pre_gate(&obs, &carry, pkg, ldconfig, emit)
                                .unwrap_or_else(|why| {
                                    warn(format!(
                                        "{pkg}: its branch work was not checked before the build: {why}"
                                    ));
                                    Default::default()
                                })
                        }
                        None => Default::default(),
                    };
                    let mut go_on = true;
                    if sent_to_pipeline(&findings, agent, &gate) {
                        match ecosystem {
                            Some(ecosystem) => {
                                let style_only = is_style_only(&findings, &gate, agent);
                                let mut lines = fixable_lines(&findings);
                                lines.extend(gate.agent.iter().cloned());
                                let item = Item {
                                    pkg: pkg.to_string(),
                                    origin: source.clone(),
                                    ecosystem,
                                    old_version: version.clone(),
                                    new_version: version,
                                    upstream_tag: None,
                                    held_version: None,
                                    dependency_of: None,
                                    requires: Vec::new(),
                                    findings: lines,
                                };
                                // A closed channel means the run gave up.
                                go_on = tx.send((item, style_only)).is_ok();
                                sent += 1;
                            }
                            None => say(format!(
                                "{pkg}: its source is not one auto-package knows, so its findings are only reported"
                            )),
                        }
                    }
                    audits.push(quality::PackageAudit {
                        pkg: pkg.to_string(),
                        findings,
                    });
                    go_on
                },
            )?;
            for line in quality::report_lines(&audits) {
                say(line);
            }
            if ldconfig_left {
                say("ldconfig scriptlets left: the target distro may not run ldconfig from a file trigger".to_string());
            }
            Ok(AuditEnd {
                packages: audits.len(),
                items: sent,
                candidates: seen.len(),
                absent,
                matched,
                unreadable,
            })
        });
        Self { items, handle }
    }

    /// How the audit ended, once `items` has run dry.
    pub(super) fn finish(self) -> Result<AuditEnd, String> {
        drop(self.items);
        self.handle
            .join()
            .unwrap_or_else(|_| Err("the audit of the project panicked".to_string()))
    }
}

/// What a lint run that audited no package found, for the names it was asked
/// for: `Some(true)` when every candidate is absent from OBS, `Some(false)` when
/// none matched, and `None` when a candidate was found, audited or skipped on
/// purpose (no spec, a maintainer's call).
pub(super) fn nothing_found(candidates: usize, absent: usize) -> Option<bool> {
    (candidates == absent).then_some(absent > 0)
}

/// Whether any finding is one the pipeline can fix (not report-only).
fn fixable(findings: &[quality::Finding]) -> bool {
    findings.iter().any(|f| !f.kind.is_report_only())
}

/// Whether an audited package goes to the pipeline: one with a finding to fix,
/// one with no finding at all when an agent runs (its review pass), one the
/// pre-pass found a tool fix or an agent fix in, when an agent runs. A package
/// whose only findings are report-only is only reported.
pub(super) fn sent_to_pipeline(
    findings: &[quality::Finding],
    agent: bool,
    gate: &super::leftovers::PreGate,
) -> bool {
    fixable(findings)
        || (agent && (findings.is_empty() || !gate.agent.is_empty()))
        || gate.has_tool()
}

/// Whether a sent package is committed without a build: its findings are style
/// slips or the pre-pass found branch leftovers for the tool, and no agent is
/// needed for them (with an agent, the pre-pass's agent findings need one).
pub(super) fn is_style_only(
    findings: &[quality::Finding],
    gate: &super::leftovers::PreGate,
    agent: bool,
) -> bool {
    let agent_needed = agent && !gate.agent.is_empty();
    !agent_needed && !quality::needs_pipeline(findings) && (fixable(findings) || gate.has_tool())
}

/// The findings an item hands the agent, one line each. Report-only findings
/// are left out: nothing in the recipe fixes them, so they are only reported.
fn fixable_lines(findings: &[quality::Finding]) -> Vec<String> {
    findings
        .iter()
        .filter(|f| !f.kind.is_report_only() && !f.kind.is_provenance())
        .map(|f| format!("{}: {}", f.kind.label(), f.detail))
        .collect()
}

/// The problem the agent is asked to fix for an `lint` item.
pub(super) fn improve_problem(item: &Item) -> Option<String> {
    if item.findings.is_empty() {
        return None;
    }
    Some(format!(
        "{} {} builds, but the quality checks of the project found these problems. Fix each \
         one in the existing recipe where it is a real defect, and keep the version and every \
         other decision. An rpmlint warning that is a false positive gets a justified \
         `rpmlintrc` filter (convention 39), not a silent skip. Do not change `%check` into a \
         disabled one to get a clean run.\n\nFindings:\n- {}",
        item.pkg,
        item.new_version,
        item.findings.join("\n- ")
    ))
}

#[cfg(test)]
mod tests {
    use super::super::leftovers::PreGate;
    use super::*;
    use ap_build::quality::{Finding, Kind};

    #[test]
    fn a_named_package_absent_from_obs_is_not_a_found_one() {
        // Nothing matched.
        assert_eq!(nothing_found(0, 0), Some(false));
        // Every named package is absent from OBS.
        assert_eq!(nothing_found(2, 2), Some(true));
        // A package skipped on purpose is found, so the run is not an error.
        assert_eq!(nothing_found(1, 0), None);
        // One found beside an absent one is found too.
        assert_eq!(nothing_found(2, 1), None);
    }

    #[test]
    fn a_package_without_findings_takes_the_agent_pass_only_when_an_agent_runs() {
        let none: [Finding; 0] = [];
        let clean = PreGate::default();
        assert!(
            sent_to_pipeline(&none, true, &clean),
            "a clean package gets its review pass"
        );
        assert!(
            !sent_to_pipeline(&none, false, &clean),
            "--no-agent keeps the package out"
        );
        let report_only = [Finding {
            kind: Kind::RpmlintrcName,
            detail: "y.rpmlintrc".to_string(),
        }];
        assert!(
            !sent_to_pipeline(&report_only, true, &clean),
            "a report-only finding is only reported"
        );
        let style = [Finding {
            kind: Kind::SpecStyle,
            detail: "a style slip".to_string(),
        }];
        assert!(
            sent_to_pipeline(&style, false, &clean),
            "a fixable finding is sent either way"
        );
    }

    #[test]
    fn a_clean_package_with_a_branch_leftover_is_sent_and_committed_without_a_build() {
        let none: [Finding; 0] = [];
        let leftover = PreGate {
            tool: vec!["changes.dependency-bullet: x".to_string()],
            agent: Vec::new(),
        };
        for agent in [true, false] {
            assert!(
                sent_to_pipeline(&none, agent, &leftover),
                "a tool fix sends the package (agent {agent})"
            );
            assert!(
                is_style_only(&none, &leftover, agent),
                "a tool fix is committed by the tidy path (agent {agent})"
            );
        }
    }

    #[test]
    fn no_agent_commits_the_tool_fixes_and_leaves_the_agent_findings_for_the_gate() {
        let none: [Finding; 0] = [];
        let mixed = PreGate {
            tool: vec!["files.leftover: old.tar.gz".to_string()],
            agent: vec!["spec.style: x".to_string()],
        };
        assert!(sent_to_pipeline(&none, false, &mixed));
        assert!(
            is_style_only(&none, &mixed, false),
            "--no-agent: the tool's cleanup is committed through the tidy path"
        );
        assert!(
            !is_style_only(&none, &mixed, true),
            "with an agent, the agent's findings go to the build instead"
        );
        assert!(sent_to_pipeline(&none, true, &mixed));
        let agent_only = PreGate {
            tool: Vec::new(),
            agent: vec!["spec.style: x".to_string()],
        };
        assert!(
            !sent_to_pipeline(&none, false, &agent_only),
            "--no-agent: an agent-only finding alone does not send the package"
        );
    }

    #[test]
    fn report_only_findings_do_not_reach_the_agent() {
        let finding = |kind, detail: &str| Finding {
            kind,
            detail: detail.to_string(),
        };
        let findings = [
            finding(Kind::RpmlintrcUnnamed, "x-rpmlintrc: not named"),
            finding(Kind::SpecStyle, "a style slip"),
            finding(Kind::RpmlintrcName, "y.rpmlintrc"),
        ];
        assert_eq!(fixable_lines(&findings), ["style: a style slip"]);
    }
}
