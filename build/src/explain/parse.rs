use super::*;

pub(super) fn parse_run(path: &Path, pkg: &str) -> Parsed {
    let mut p = Parsed::default();
    let Ok(text) = std::fs::read_to_string(path) else {
        return p;
    };
    let needle = format!("\"pkg\":\"{pkg}\"");
    let mut agent_turns = 0;
    for line in text.lines() {
        let run_level = line.starts_with("{\"event\":\"run_started\"")
            || line.starts_with("{\"event\":\"run_finished\"")
            || line.starts_with("{\"event\":\"host_allowed\"");
        let arch_check = line.starts_with("{\"event\":\"arch_check\"");
        if !run_level && !arch_check && !line.contains(&needle) {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            p.bad_lines += 1;
            continue;
        };
        // `arch_check` names its package in `package`, the others in `pkg`.
        let owner = v["pkg"].as_str().or(v["package"].as_str());
        if !run_level && owner != Some(pkg) {
            continue;
        }
        let step = v["step"].as_u64();
        let attempt =
            |p: &mut Parsed| step.and_then(|s| p.attempts.iter().rposition(|a| a.step == s));
        match v["event"].as_str().unwrap_or("") {
            "run_started" => {
                p.argv = v["argv"].as_array().map(|a| {
                    a.iter()
                        .filter_map(|x| x.as_str().map(str::to_string))
                        .collect()
                });
                p.build = v["build"].as_str().map(str::to_string);
            }
            "run_finished" => p.run_finished = Some(v),
            "host_allowed" => p.hosts_allowed.push(v),
            "arch_check" => p.arch_checks.push(v),
            "target_selected" => p.selected.push(v),
            "step_started" => p.attempts.push(Attempt {
                step: step.unwrap_or(0),
                kind: label(&v["kind"]),
                attempt: v["attempt"].as_u64(),
                round: v["round"].as_u64(),
                detail: v["detail"].as_str().map(str::to_string),
                ..Default::default()
            }),
            "step_finished" => {
                if let Some(i) = attempt(&mut p) {
                    p.attempts[i].outcome = Some(label(&v["outcome"]));
                    p.attempts[i].elapsed_ms = v["elapsed"].as_u64();
                }
            }
            "step_skipped" => {
                let reason = match &v["reason"] {
                    Value::Object(m) => m.values().next().map(|x| label(x)).unwrap_or_default(),
                    other => label(other),
                };
                if label(&v["kind"]) == "AgentTurn" {
                    p.skipped.push(format!("agent turn skipped: {reason}"));
                }
            }
            "agent_prompt" => {
                if let Some(i) = attempt(&mut p) {
                    agent_turns += 1;
                    p.attempts[i].agent_turn = Some(agent_turns);
                    let task = match &v["task"] {
                        Value::Object(m) => m
                            .iter()
                            .next()
                            .map(|(k, x)| {
                                match (x["attempt"].as_u64(), x["max_attempts"].as_u64()) {
                                    (Some(a), Some(m)) => format!("{k} {a}/{m}"),
                                    _ => k.clone(),
                                }
                            })
                            .unwrap_or_default(),
                        other => label(other),
                    };
                    p.attempts[i].agent.get_or_insert_with(Agent::default).task = Some(task);
                }
            }
            "agent_reply" => {
                if let Some(i) = attempt(&mut p) {
                    let r = &v["response"];
                    let g = p.attempts[i].agent.get_or_insert_with(Agent::default);
                    g.action = r["action"].as_str().map(str::to_string);
                    g.reasoning = r["reasoning"].as_str().map(str::to_string);
                    g.confidence = r["confidence"].as_str().map(str::to_string);
                    g.evidence = r["evidence"]
                        .as_array()
                        .map(|a| {
                            a.iter()
                                .filter_map(|x| x.as_str().map(str::to_string))
                                .collect()
                        })
                        .unwrap_or_default();
                    g.cost_usd = v["usage"]["cost_usd"].as_f64();
                    g.input_tokens = v["usage"]["input_tokens"].as_u64();
                    g.output_tokens = v["usage"]["output_tokens"].as_u64();
                }
            }
            "spec_changed" => {
                if let Some(i) = attempt(&mut p) {
                    let a = &mut p.attempts[i];
                    a.spec_added =
                        Some(a.spec_added.unwrap_or(0) + v["lines_added"].as_u64().unwrap_or(0));
                    a.spec_removed = Some(
                        a.spec_removed.unwrap_or(0) + v["lines_removed"].as_u64().unwrap_or(0),
                    );
                }
            }
            "spec_rejected" => {
                let why = rejection(&v["reason"]);
                if let Some(i) = attempt(&mut p) {
                    p.attempts[i].rejected = Some(why.clone());
                }
                p.rejected.push((step, why));
            }
            "build_failed" => {
                p.build_log = v["log"].as_str().map(str::to_string);
                if let Some(i) = attempt(&mut p) {
                    p.attempts[i]
                        .build
                        .get_or_insert_with(BuildInfo::default)
                        .classification = v["classification"].as_str().map(str::to_string);
                }
            }
            "missing_packages" => {
                if let Some(i) = attempt(&mut p) {
                    let names: Vec<String> = v["names"]
                        .as_array()
                        .map(|a| {
                            a.iter()
                                .filter_map(|x| x.as_str().map(str::to_string))
                                .collect()
                        })
                        .unwrap_or_default();
                    p.attempts[i]
                        .build
                        .get_or_insert_with(BuildInfo::default)
                        .missing = names;
                }
            }
            "extract_result" => {
                if let Some(i) = attempt(&mut p) {
                    let e = &v["extract"];
                    let s = format!(
                        "{} (found by {}, genre {})",
                        e["phase"].as_str().unwrap_or("?"),
                        e["located_by"].as_str().unwrap_or("?"),
                        e["genre"].as_str().unwrap_or("?")
                    );
                    p.attempts[i]
                        .build
                        .get_or_insert_with(BuildInfo::default)
                        .extracts
                        .push(s);
                }
            }
            "notes_usage" => p.notes_cost += v["usage"]["cost_usd"].as_f64().unwrap_or(0.0),
            "failed_turn_usage" => {
                p.failed_turns += 1;
                p.failed_turn_cost += v["usage"]["cost_usd"].as_f64().unwrap_or(0.0);
            }
            "warning" => {
                if let Some(m) = v["message"].as_str() {
                    p.warnings.push((step, m.to_string()));
                }
            }
            "package_finished" => p.finished = Some(v),
            _ => {}
        }
    }
    p
}

pub(super) fn rejection(reason: &Value) -> String {
    let Value::Object(m) = reason else {
        return label(reason);
    };
    let Some((kind, detail)) = m.iter().next() else {
        return "spec rejected".to_string();
    };
    let extra = match kind.as_str() {
        "Placeholders" => detail["lines"].as_array().map(|a| {
            a.iter()
                .filter_map(|x| x.as_str())
                .collect::<Vec<_>>()
                .join("; ")
        }),
        "RpmspecParseFailed" => detail["stderr"]
            .as_str()
            .and_then(|s| s.lines().find(|l| l.starts_with("error:")))
            .map(|l| l.rsplit_once(": ").map_or(l, |(_, tail)| tail).to_string()),
        _ => (!detail.is_null()).then(|| detail.to_string()),
    };
    match extra {
        Some(e) if !e.is_empty() => format!("{kind}: {}", truncate(&e, 200)),
        _ => kind.clone(),
    }
}

pub(super) fn command_line(argv: &[String]) -> String {
    let mut it = argv.iter().skip(1);
    let mut out = String::new();
    while let Some(a) = it.next() {
        if !out.is_empty() {
            out.push(' ');
        }
        out.push_str(a);
        // Long paths of the work dir add nothing.
        if a == "--work-dir"
            && let Some(_) = it.next()
        {
            out.push_str(" ...");
        }
    }
    out
}

pub(super) fn flag_value(argv: &[String], flag: &str) -> Option<String> {
    let eq = format!("{flag}=");
    let mut it = argv.iter();
    while let Some(a) = it.next() {
        if a == flag {
            return it.next().cloned();
        }
        if let Some(v) = a.strip_prefix(&eq) {
            return Some(v.to_string());
        }
    }
    None
}

pub(super) fn budget(argv: Option<&[String]>) -> (f64, bool) {
    argv.and_then(|a| flag_value(a, "--budget-usd"))
        .and_then(|v| v.parse::<f64>().ok())
        .map_or((DEFAULT_BUDGET_USD, true), |b| (b, false))
}

pub(super) fn describe_reason(raw: &str) -> String {
    match raw {
        "ExplicitOverride" => "the package was named explicitly for this run (explicit override)",
        "FirstUnbuiltDependency" => "first unbuilt dependency in the build order",
        "RequestedRoot" => "requested as a root of the build",
        other => return other.to_string(),
    }
    .to_string()
}

pub(super) fn version_info(p: &Parsed, dir: &Path, obs_name: &str) -> VersionInfo {
    let mut v = VersionInfo {
        selections: p.selected.len(),
        ..Default::default()
    };
    if let Some(s) = p.selected.last() {
        v.to = s["version"].as_str().map(str::to_string);
        v.from = s["from_version"].as_str().map(str::to_string);
        let raw = label(&s["reason"]);
        v.reason = Some(describe_reason(&raw));
        v.reason_raw = Some(raw);
        v.blocked_by = s["blocked_by"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
    }
    if let Some(argv) = &p.argv {
        // The run's `--only` limits which packages the flags belong to; only
        // claim them when the run was about this package.
        v.revision = flag_value(argv, "--revision");
        v.forced_version = flag_value(argv, "--to-version");
    }
    if let Some((spec, _)) = checkout(dir, obs_name) {
        v.spec_version = std::fs::read_to_string(spec).ok().and_then(|t| {
            t.lines()
                .find_map(|l| l.strip_prefix("Version:"))
                .map(|x| x.trim().to_string())
        });
    }
    v
}

/// The checkout's spec and `.changes` (the directory is named after the OBS
/// project, so look in every subdirectory that is not the tool's own).
pub(super) fn checkout(dir: &Path, obs_name: &str) -> Option<(PathBuf, PathBuf)> {
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if matches!(name.as_str(), "state" | "runs" | "rpms") || !e.path().is_dir() {
            continue;
        }
        let pkg_dir = e.path().join(obs_name);
        let spec = pkg_dir.join(format!("{obs_name}.spec"));
        if spec.is_file() {
            return Some((spec, pkg_dir.join(format!("{obs_name}.changes"))));
        }
    }
    None
}

pub(super) fn latest_changes_entry(path: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut lines = text.lines().peekable();
    // Skip the leading separator, then take up to the next one.
    if lines.peek().is_some_and(|l| l.starts_with("-----")) {
        lines.next();
    }
    lines
        .take_while(|l| !l.starts_with("-----"))
        .filter(|l| !l.trim().is_empty())
        .take(14)
        .map(|l| truncate(l, 200))
        .collect()
}

pub(super) fn step_dir(run_pkg: &Path, step: u64) -> Option<PathBuf> {
    // Directories are `<step + 1>-<kind>`, zero-padded in newer runs.
    std::fs::read_dir(run_pkg).ok()?.flatten().find_map(|e| {
        let name = e.file_name().to_string_lossy().into_owned();
        let n: u64 = name.split('-').next()?.parse().ok()?;
        (n == step + 1 && e.path().is_dir()).then(|| e.path())
    })
}

pub(super) fn outcome(
    p: &Parsed,
    state: Option<&Value>,
    dir: &Path,
    obs_name: &str,
    pkg: &str,
    run_pkg: Option<&Path>,
) -> Outcome {
    let mut o = Outcome {
        state_outcome: state
            .and_then(|s| s["last_outcome"].as_str())
            .map(str::to_string),
        state_reason: state
            .and_then(|s| s["last_failure_reason"].as_str())
            .map(str::to_string),
        ..Default::default()
    };
    match p.finished.as_ref().map(|f| &f["outcome"]) {
        Some(Value::String(s)) if s == "Built" => o.result = "built".into(),
        Some(Value::String(s)) if s == "AlreadyBuilt" => {
            o.result = "already built".into();
            o.detail = Some("nothing needed to be done in this run".into());
        }
        Some(Value::Object(m)) => {
            let (k, v) = m
                .iter()
                .next()
                .map(|(k, v)| (k.as_str(), v))
                .unwrap_or(("", &Value::Null));
            match k {
                "MissingDeps" => {
                    o.result = "missing dependencies".into();
                    let deps: Vec<&str> = v["deps"]
                        .as_array()
                        .map(|a| a.iter().filter_map(|x| x.as_str()).collect())
                        .unwrap_or_default();
                    o.detail = Some(format!("waiting for: {}", deps.join(", ")));
                }
                "Failed" => {
                    o.result = "failed".into();
                    o.detail = v["reason"].as_str().map(str::to_string);
                }
                "Blocked" => {
                    o.result = "blocked".into();
                    o.detail = v["reason"].as_str().map(str::to_string);
                }
                other => o.result = other.to_string(),
            }
        }
        _ => o.result = "no result recorded".into(),
    }
    let shorten = |t: String| t.replace(&dir.display().to_string(), "<work dir>");
    o.detail = o.detail.take().map(shorten);
    o.state_reason = o.state_reason.take().map(shorten);
    if p.finished.is_none() {
        o.detail = Some(
            "this run wrote no package_finished event for it: it was interrupted, or is still going"
                .into(),
        );
    }
    if matches!(
        o.result.as_str(),
        "failed" | "no result recorded" | "missing dependencies"
    ) {
        let candidates = [
            p.build_log.as_ref().map(PathBuf::from),
            run_pkg.and_then(last_build_log),
            Some(dir.join("state").join(pkg).join("build.log")),
        ];
        for c in candidates.into_iter().flatten() {
            if let Some(text) = read_tail(&c) {
                let errors = log_errors(&text);
                if !errors.is_empty() {
                    o.log_source = Some(c.strip_prefix(dir).unwrap_or(&c).display().to_string());
                    o.log_errors = errors;
                    break;
                }
            }
        }
    }
    if let Some((_, changes)) = checkout(dir, obs_name) {
        o.changelog = latest_changes_entry(&changes);
    }
    o
}
