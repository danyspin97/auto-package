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
}

impl Audit {
    pub(super) fn start(a: &UpdateProjectArgs, reporter: Arc<dyn Reporter + Send + Sync>) -> Self {
        let (tx, items) = std::sync::mpsc::channel();
        let (source, distro, arch) = (a.source.clone(), a.distro.clone(), a.arch.clone());
        let (only, exclude) = (a.only.clone(), a.exclude.clone());
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
            let mut ldconfig_left = false;
            update::plan_each(
                &source,
                &|n| wanted_by(&only, &exclude, n),
                emit,
                &mut |c| {
                    let pkg = up::branch_name(&c.rpm_name);
                    if !seen.insert(pkg.to_string()) {
                        return true;
                    }
                    let Ok(spec) = update::planned_spec(&source, pkg, emit) else {
                        warn(format!(
                            "could not read the spec of {pkg}, so it is not checked"
                        ));
                        return true;
                    };
                    ldconfig_left |= ap_build::spec_tidy::hygiene::ldconfig_left(&spec, ldconfig);
                    let mut findings = quality::audit_spec(&spec, ldconfig);
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
                    let mut go_on = true;
                    if !findings.is_empty() {
                        match ecosystem {
                            Some(ecosystem) => {
                                let style_only = !quality::needs_pipeline(&findings);
                                let item = Item {
                                    pkg: pkg.to_string(),
                                    origin: source.clone(),
                                    ecosystem,
                                    old_version: version.clone(),
                                    new_version: version,
                                    dependency_of: None,
                                    findings: findings
                                        .iter()
                                        .map(|f| format!("{}: {}", f.kind.label(), f.detail))
                                        .collect(),
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
                say("ldconfig scriptlets left: the target distro may not define %ldconfig_scriptlets".to_string());
            }
            Ok(AuditEnd {
                packages: audits.len(),
                items: sent,
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
