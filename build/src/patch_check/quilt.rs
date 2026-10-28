use super::*;

/// Where the quilt binary is, or `None`.
pub(super) fn quilt_available() -> bool {
    std::process::Command::new("quilt")
        .arg("--version")
        .env("QUILTRC", "/dev/null")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// A report with every planned patch not checked, for `why`.
pub(super) fn not_run(plan: &[Planned], why: String) -> CheckReport {
    CheckReport {
        patches: plan
            .iter()
            .map(|p| match p {
                Planned::Check(e) => PatchResult {
                    name: e.name.clone(),
                    level: e.level,
                    status: PatchStatus::NotChecked {
                        reason: why.clone(),
                    },
                    refreshed: None,
                },
                Planned::Skip { name, reason } => PatchResult {
                    name: name.clone(),
                    level: 1,
                    status: PatchStatus::NotChecked {
                        reason: reason.clone(),
                    },
                    refreshed: None,
                },
            })
            .collect(),
        not_run: Some(why),
    }
}

/// One quilt invocation in the scratch tree.
pub(super) struct Quilt {
    pub(super) root: PathBuf,
    pub(super) patches: PathBuf,
    pub(super) env: std::collections::BTreeMap<String, String>,
    pub(super) scratch_text: String,
}

impl Quilt {
    pub(super) fn run(&self, args: &[&str]) -> Result<(bool, String), String> {
        let out = crate::proc::capture_with_timeout_env(
            "quilt",
            args,
            &self.root,
            crate::proc::Emit::quiet(),
            Duration::from_secs(120),
            Some(&self.env),
        )
        .map_err(|e| format!("could not run quilt {}: {e}", args.join(" ")))?;
        let text = format!("{}{}", out.stdout, out.stderr).replace(&self.scratch_text, "");
        Ok((out.success(), text))
    }

    pub(super) fn top(&self) -> Option<String> {
        self.run(&["top"])
            .ok()
            .filter(|(ok, _)| *ok)
            .map(|(_, t)| t.trim().rsplit('/').next().unwrap_or("").to_string())
    }
}
