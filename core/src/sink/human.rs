use super::*;

pub struct HumanSink {
    level: LogLevel,
}

impl HumanSink {
    pub fn new(level: LogLevel) -> Self {
        Self { level }
    }

    fn event_level(event: &Event) -> LogLevel {
        match event {
            Event::RunFinished { outcome, .. } => match outcome {
                RunOutcome::Success => LogLevel::Info,
                _ => LogLevel::Error,
            },
            Event::BuildFailed { .. } => LogLevel::Error,
            Event::Warning { .. } => LogLevel::Warn,
            Event::MissingPackages { .. } => LogLevel::Warn,
            Event::SpecRejected { .. } => LogLevel::Warn,
            Event::RunStarted { .. }
            | Event::ResolveStarted { .. }
            | Event::ResolveFinished { .. }
            | Event::TargetSelected { .. }
            | Event::StepStarted { .. }
            | Event::StepFinished { .. }
            | Event::StepSkipped { .. }
            | Event::SpecChanged { .. }
            | Event::PackageFinished { .. }
            | Event::NotesUsage { .. }
            | Event::FailedTurnUsage { .. }
            | Event::Progress { .. } => LogLevel::Info,
            Event::Command { .. }
            | Event::CommandDone { .. }
            | Event::ExtractResult { .. }
            | Event::AgentPrompt { .. }
            | Event::AgentReply { .. }
            | Event::AgentToolUse { .. }
            | Event::Artifact { .. } => LogLevel::Debug,
        }
    }

    fn format(&self, event: &Event) -> Option<String> {
        match event {
            Event::RunStarted {
                run_id, work_dir, ..
            } => Some(format!(
                "{} · run {} · {}",
                "auto-package".if_supports_color(Stderr, |t| t.bold()),
                run_id.if_supports_color(Stderr, |t| t.dimmed()),
                work_dir.display().if_supports_color(Stderr, |t| t.dimmed()),
            )),
            Event::RunFinished {
                outcome,
                elapsed,
                usage,
            } => {
                let outcome_colored = match outcome {
                    RunOutcome::Success => format!(
                        "{}",
                        "✓ run finished"
                            .if_supports_color(Stderr, |t| t.style(Style::new().green().bold()))
                    ),
                    _ => format!(
                        "{}",
                        "✗ run failed"
                            .if_supports_color(Stderr, |t| t.style(Style::new().red().bold()))
                    ),
                };
                Some(format!(
                    "\n{outcome_colored} in {} · {}",
                    fmt_elapsed(*elapsed),
                    format!("${:.2}", usage.cost_usd).if_supports_color(Stderr, |t| t.dimmed()),
                ))
            }
            Event::ResolveStarted { root } => Some(format!(
                "{} resolve {}",
                "◆".if_supports_color(Stderr, |t| t.cyan()),
                root.if_supports_color(Stderr, |t| t.bold()),
            )),
            Event::ResolveFinished {
                total,
                built,
                in_distro,
                remaining,
            } => Some(format!(
                "  {} packages in closure · {} already built · {} in the distro · {} remaining",
                total,
                built.if_supports_color(Stderr, |t| t.green()),
                in_distro.if_supports_color(Stderr, |t| t.dimmed()),
                remaining,
            )),
            Event::TargetSelected {
                pkg,
                version,
                blocked_by,
                ..
            } => {
                let mut s = format!(
                    "\n{} {} {}",
                    "▶".if_supports_color(Stderr, |t| t.style(Style::new().cyan().bold())),
                    pkg.if_supports_color(Stderr, |t| t.bold()),
                    version.if_supports_color(Stderr, |t| t.dimmed()),
                );
                if !blocked_by.is_empty() {
                    s.push_str(&format!(
                        "  {}",
                        format!("(blocked by: {})", blocked_by.join(", "))
                            .if_supports_color(Stderr, |t| t.yellow()),
                    ));
                }
                Some(s)
            }
            Event::PackageFinished {
                pkg,
                version,
                outcome,
            } => {
                let outcome_colored = match outcome {
                    PackageOutcome::Built => format!(
                        "{}",
                        "✓ built"
                            .if_supports_color(Stderr, |t| t.style(Style::new().green().bold()))
                    ),
                    PackageOutcome::BuiltLocally => format!(
                        "{}",
                        "✓ built locally"
                            .if_supports_color(Stderr, |t| t.style(Style::new().green().bold()))
                    ),
                    PackageOutcome::AlreadyBuilt => format!(
                        "{}",
                        "✔ already built".if_supports_color(Stderr, |t| t.green())
                    ),
                    PackageOutcome::MissingDeps { deps } => format!(
                        "{}",
                        format!("◇ blocked on {}", deps.join(", "))
                            .if_supports_color(Stderr, |t| t.yellow()),
                    ),
                    PackageOutcome::Failed { reason, .. } => format!(
                        "{}",
                        format!("✗ failed: {reason}")
                            .if_supports_color(Stderr, |t| t.style(Style::new().red().bold())),
                    ),
                };
                Some(format!(
                    "{outcome_colored}  {} {}",
                    pkg.if_supports_color(Stderr, |t| t.bold()),
                    version.if_supports_color(Stderr, |t| t.dimmed()),
                ))
            }
            Event::StepStarted { kind, .. } => Some(format!(
                "  {} {}",
                "▸".if_supports_color(Stderr, |t| t.cyan()),
                kind.label().if_supports_color(Stderr, |t| t.bold()),
            )),
            Event::StepSkipped { kind, reason } => Some(format!(
                "  {}",
                format!("⇥ skipped {}: {reason}", kind.label())
                    .if_supports_color(Stderr, |t| t.yellow()),
            )),
            Event::StepFinished {
                outcome, elapsed, ..
            } => {
                let outcome_colored = match outcome {
                    StepOutcome::Ok => format!("{}", "✓".if_supports_color(Stderr, |t| t.green())),
                    StepOutcome::Failed => {
                        format!("{}", "✗ failed".if_supports_color(Stderr, |t| t.red()))
                    }
                    StepOutcome::Skipped => {
                        format!("{}", "⇥ skipped".if_supports_color(Stderr, |t| t.yellow()))
                    }
                };
                Some(format!(
                    "    {outcome_colored} {}",
                    fmt_elapsed(*elapsed).if_supports_color(Stderr, |t| t.dimmed()),
                ))
            }
            Event::BuildFailed {
                classification,
                log,
                ..
            } => {
                let label = match classification {
                    crate::event::BuildFailure::RpmParseError { message, .. } => {
                        format!("rpm parse error: {message}")
                    }
                    crate::event::BuildFailure::PrepFailed => "prep failed".to_string(),
                    crate::event::BuildFailure::BuildFailed => "build failed".to_string(),
                    crate::event::BuildFailure::InstallFailed => "install failed".to_string(),
                    crate::event::BuildFailure::CheckFailed => "check failed".to_string(),
                    crate::event::BuildFailure::UnresolvableDeps { packages } => {
                        format!("unresolvable deps: {}", packages.join(", "))
                    }
                    crate::event::BuildFailure::Unclassified { raw } => {
                        format!("failed: {raw}")
                    }
                };
                Some(format!(
                    "  {} {label}\n    log: {}",
                    "✗ FAILED".if_supports_color(Stderr, |t| t.style(Style::new().red().bold())),
                    log.display().if_supports_color(Stderr, |t| t.dimmed()),
                ))
            }
            Event::Progress { message, .. } => Some(format!(
                "     {}",
                message.if_supports_color(Stderr, |t| t.dimmed()),
            )),
            Event::Warning { message, .. } => Some(format!(
                "  {} {message}",
                "⚠ warning:".if_supports_color(Stderr, |t| t.style(Style::new().yellow().bold())),
            )),
            Event::MissingPackages { names, .. } => Some(format!(
                "  {} {}",
                "⚠ missing packages:".if_supports_color(Stderr, |t| t.yellow()),
                names.join(", "),
            )),
            Event::SpecChanged {
                lines_added,
                lines_removed,
                diff,
                artifact,
                ..
            } => {
                if *lines_added == 0 && *lines_removed == 0 {
                    return None;
                }
                let mut s = format!(
                    "  spec: {} {}",
                    format!("+{lines_added}").if_supports_color(Stderr, |t| t.green()),
                    format!("-{lines_removed}").if_supports_color(Stderr, |t| t.red()),
                );
                if self.level >= LogLevel::Debug {
                    s.push('\n');
                    s.push_str(&delimited("spec diff", diff, artifact));
                }
                Some(s)
            }
            Event::Command {
                program,
                argv,
                cwd,
                log,
                ..
            } => {
                let mut s = format!(
                    "  {} {} {} {}",
                    "+".if_supports_color(Stderr, |t| t.dimmed()),
                    program.if_supports_color(Stderr, |t| t.bold()),
                    argv.join(" ").if_supports_color(Stderr, |t| t.dimmed()),
                    format!("(in {})", cwd.display()).if_supports_color(Stderr, |t| t.dimmed()),
                );
                if let Some(log_path) = log {
                    s.push_str(&format!(
                        "\n    log: {}",
                        log_path.display().if_supports_color(Stderr, |t| t.dimmed()),
                    ));
                }
                Some(s)
            }
            Event::AgentPrompt { task, artifact, .. } => Some(format!(
                "  {}: {task} → {}",
                "agent".if_supports_color(Stderr, |t| t.style(Style::new().cyan().bold())),
                artifact.display().if_supports_color(Stderr, |t| t.dimmed()),
            )),
            Event::ExtractResult {
                extract, artifact, ..
            } => {
                let mut body = format!(
                    "genre: {}\nphase: {}\nlocated by: {}\nelided: {} line(s)\n",
                    extract.genre.as_deref().unwrap_or("—"),
                    extract.phase.as_deref().unwrap_or("—"),
                    extract.located_by.as_deref().unwrap_or("—"),
                    extract.elided_count,
                );
                if !extract.has_signal {
                    body.push_str("extractor found no signal — showing the log tail\n");
                }
                if !extract.source_candidates.is_empty() {
                    body.push_str("source candidates:\n");
                    for c in &extract.source_candidates {
                        body.push_str(&format!("  {}\n", c.display()));
                    }
                }
                if !extract.error_lines.is_empty() {
                    body.push_str("error lines:\n");
                    for l in &extract.error_lines {
                        body.push_str(&format!("  {l}\n"));
                    }
                }
                body.push_str("excerpt:\n");
                body.push_str(&extract.excerpt);
                Some(delimited("extract", &body, artifact))
            }
            Event::AgentToolUse { tool, input, .. } => {
                let summary: String = input.chars().take(160).collect();
                Some(format!(
                    "    {} {tool} {}",
                    "→".if_supports_color(Stderr, |t| t.dimmed()),
                    summary.if_supports_color(Stderr, |t| t.dimmed()),
                ))
            }
            Event::Artifact { name, path, .. } => Some(format!(
                "    {name}: {}",
                path.display().if_supports_color(Stderr, |t| t.dimmed()),
            )),
            Event::AgentReply {
                response,
                usage,
                raw,
                artifact,
                ..
            } => {
                let confidence = response.confidence.as_deref().unwrap_or("?");
                let confidence_colored = match confidence {
                    "High" => format!("{}", confidence.if_supports_color(Stderr, |t| t.green())),
                    "Medium" => format!("{}", confidence.if_supports_color(Stderr, |t| t.yellow())),
                    _ => format!("{}", confidence.if_supports_color(Stderr, |t| t.red())),
                };
                let action_colored = match response.action.as_str() {
                    "Edited" => format!(
                        "{}",
                        response.action.if_supports_color(Stderr, |t| t.green())
                    ),
                    "NoChange" => format!(
                        "{}",
                        response.action.if_supports_color(Stderr, |t| t.yellow())
                    ),
                    _ => format!("{}", response.action.if_supports_color(Stderr, |t| t.red())),
                };
                let reasoning = if response.reasoning.chars().count() > 120 {
                    format!(
                        "{}…",
                        response.reasoning.chars().take(120).collect::<String>()
                    )
                } else {
                    response.reasoning.clone()
                };
                let mut s = format!(
                    "  {} ({action_colored} · {confidence_colored} · {}): {reasoning}",
                    "reply".if_supports_color(Stderr, |t| t.cyan()),
                    format!("${:.2}", usage.cost_usd).if_supports_color(Stderr, |t| t.dimmed()),
                );
                if self.level >= LogLevel::Trace && !raw.is_empty() {
                    s.push('\n');
                    s.push_str(&delimited("agent response", raw, artifact));
                }
                Some(s)
            }
            _ => None,
        }
    }
}

/// A duration for people: `850ms`, `12.3s`, `4m07s`, `1h02m`.
pub(super) fn fmt_elapsed(d: Duration) -> String {
    let ms = d.as_millis();
    match ms {
        0..1000 => format!("{ms}ms"),
        1000..60_000 => format!("{:.1}s", ms as f64 / 1000.0),
        60_000..3_600_000 => format!("{}m{:02}s", ms / 60_000, ms / 1000 % 60),
        _ => format!("{}h{:02}m", ms / 3_600_000, ms / 60_000 % 60),
    }
}

/// A grep-able dump: a titled rule, the body, and a footer naming the file it
/// was also written to. Every `--log debug` dump is delimited this way.
fn delimited(title: &str, body: &str, artifact: &Path) -> String {
    let rule = "─".repeat(58usize.saturating_sub(title.len()));
    format!(
        "── {title} ── {rule}\n{}\n── end {title} ── also at {}",
        body.trim_end(),
        artifact.display(),
    )
}

impl Reporter for HumanSink {
    fn report(&self, event: &Event) {
        let level = Self::event_level(event);
        if level > self.level {
            return;
        }
        if let Some(msg) = self.format(event) {
            let _ = writeln!(std::io::stderr(), "{}", strip_controls(&msg, true));
        }
    }
}
