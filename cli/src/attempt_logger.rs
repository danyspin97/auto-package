//! Real-time attempt logging for TUI visibility.
//!
//! Instead of saving all attempt data at the end, this module writes files
//! incrementally so the TUI can pick them up immediately.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptState {
    InProgress,
    Completed,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptPhase {
    Starting,
    CallingAgent,
    Validating,
    Done,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttemptStatus {
    pub number: usize,
    pub kind: String,
    pub section: String,
    pub state: AttemptState,
    pub phase: AttemptPhase,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    pub started_at: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_preview: Option<String>,
}

/// A live attempt being logged incrementally.
pub struct AttemptLogger {
    dir: PathBuf,
    status: AttemptStatus,
}

impl AttemptLogger {
    /// Create a new attempt directory and mark it as in-progress.
    pub fn start(fixes_dir: &Path, pkg: &str, slot: &str) -> std::io::Result<Self> {
        let dir = fixes_dir.join(pkg.to_lowercase()).join(slot);
        fs::create_dir_all(&dir)?;

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        let number = slot.parse::<usize>().unwrap_or(0);

        let status = AttemptStatus {
            number,
            kind: String::new(),
            section: String::new(),
            state: AttemptState::InProgress,
            phase: AttemptPhase::Starting,
            confidence: None,
            reasoning: None,
            action: None,
            started_at: now,
            finished_at: None,
            error_preview: None,
        };

        let mut logger = Self { dir, status };
        logger.write_status()?;

        // Keep legacy status.txt for backward compatibility
        fs::write(logger.dir.join("status.txt"), "in_progress")?;

        Ok(logger)
    }

    fn write_status(&self) -> std::io::Result<()> {
        let json = serde_json::to_string_pretty(&self.status)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
        fs::write(self.dir.join("status.json"), json)
    }

    fn update_status<F>(&mut self, f: F)
    where
        F: FnOnce(&mut AttemptStatus),
    {
        f(&mut self.status);
        if let Err(e) = self.write_status() {
            eprintln!("warning: failed to write status.json: {e}");
        }
    }

    /// Set the phase (starting, calling_agent, validating, done).
    pub fn set_phase(&mut self, phase: AttemptPhase) {
        self.update_status(|s| s.phase = phase);
    }

    /// Write the attempt kind (review, build, rpmspec, test).
    pub fn write_kind(&mut self, kind: &str) {
        if let Err(e) = fs::write(self.dir.join("kind.txt"), kind) {
            eprintln!("warning: failed to write kind.txt: {e}");
        }
        self.update_status(|s| s.kind = kind.to_string());
    }

    /// Write the failed section (%install, %check, etc).
    pub fn write_section(&mut self, section: &str) {
        if let Err(e) = fs::write(self.dir.join("section.txt"), section) {
            eprintln!("warning: failed to write section.txt: {e}");
        }
        self.update_status(|s| s.section = section.to_string());
    }

    /// Write the escalation level (error-context, build-tree, etc).
    pub fn write_escalation(&self, escalation: &str) {
        if let Err(e) = fs::write(self.dir.join("escalation.txt"), escalation) {
            eprintln!("warning: failed to write escalation.txt: {e}");
        }
    }

    /// Write the error excerpt from the build log.
    pub fn write_error(&mut self, error: &str) {
        if let Err(e) = fs::write(self.dir.join("error.txt"), error) {
            eprintln!("warning: failed to write error.txt: {e}");
        }
        // Store first 3 lines as preview
        let preview: Vec<&str> = error.lines().take(3).collect();
        if !preview.is_empty() {
            self.update_status(|s| s.error_preview = Some(preview.join("\n")));
        }
    }

    /// Write the prompt that will be sent to Claude.
    /// Call this BEFORE invoking the agent so TUI can show it immediately.
    pub fn write_prompt(&self, prompt: &str) {
        if let Err(e) = fs::write(self.dir.join("prompt.txt"), prompt) {
            eprintln!("warning: failed to write prompt.txt: {e}");
        }
    }

    /// Write the spec before the fix attempt.
    pub fn write_spec_before(&self, spec: &str) {
        if let Err(e) = fs::write(self.dir.join("spec_before.spec"), spec) {
            eprintln!("warning: failed to write spec_before.spec: {e}");
        }
    }

    /// Write Claude's response after it returns.
    pub fn write_response(&mut self, reasoning: &str, confidence: &str, action: &str) {
        if let Err(e) = fs::write(self.dir.join("reasoning.txt"), reasoning) {
            eprintln!("warning: failed to write reasoning.txt: {e}");
        }
        if let Err(e) = fs::write(self.dir.join("confidence.txt"), confidence) {
            eprintln!("warning: failed to write confidence.txt: {e}");
        }
        if let Err(e) = fs::write(self.dir.join("action.txt"), action) {
            eprintln!("warning: failed to write action.txt: {e}");
        }

        // Write structured response.txt
        let response = format!(
            "Confidence: {}\nAction: {}\nReasoning: {}\n",
            confidence, action, reasoning
        );
        if let Err(e) = fs::write(self.dir.join("response.txt"), response) {
            eprintln!("warning: failed to write response.txt: {e}");
        }

        // Update status
        self.update_status(|s| {
            s.confidence = Some(confidence.to_string());
            s.reasoning = Some(reasoning.to_string());
            s.action = Some(action.to_string());
        });
    }

    /// Write missing packages list.
    pub fn write_missing_packages(&self, packages: &[String]) {
        if !packages.is_empty() {
            if let Err(e) = fs::write(self.dir.join("missing_packages.txt"), packages.join("\n")) {
                eprintln!("warning: failed to write missing_packages.txt: {e}");
            }
        }
    }

    /// Write the spec after the fix attempt.
    pub fn write_spec_after(&self, spec: &str) {
        if let Err(e) = fs::write(self.dir.join("spec_after.spec"), spec) {
            eprintln!("warning: failed to write spec_after.spec: {e}");
        }
    }

    /// Write the diff between before and after.
    pub fn write_diff(&self, spec_before: &str, spec_after: &str) {
        let mut diff = String::new();
        for d in diff::lines(spec_before, spec_after) {
            match d {
                diff::Result::Left(l) => {
                    diff.push_str("- ");
                    diff.push_str(l);
                    diff.push('\n');
                }
                diff::Result::Right(r) => {
                    diff.push_str("+ ");
                    diff.push_str(r);
                    diff.push('\n');
                }
                diff::Result::Both(_, _) => {}
            }
        }
        if let Err(e) = fs::write(self.dir.join("diff.txt"), diff) {
            eprintln!("warning: failed to write diff.txt: {e}");
        }
    }

    /// Mark the attempt as completed.
    pub fn complete(mut self) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        self.update_status(|s| {
            s.state = AttemptState::Completed;
            s.phase = AttemptPhase::Done;
            s.finished_at = Some(now);
        });

        if let Err(e) = fs::write(self.dir.join("status.txt"), "completed") {
            eprintln!("warning: failed to write status.txt: {e}");
        }
    }

    /// Mark the attempt as failed.
    pub fn fail(mut self, reason: &str) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        self.update_status(|s| {
            s.state = AttemptState::Failed;
            s.phase = AttemptPhase::Done;
            s.finished_at = Some(now);
            if s.error_preview.is_none() {
                s.error_preview = Some(reason.to_string());
            }
        });

        if let Err(e) = fs::write(self.dir.join("status.txt"), "failed") {
            eprintln!("warning: failed to write status.txt: {e}");
        }
        if let Err(e) = fs::write(self.dir.join("failure_reason.txt"), reason) {
            eprintln!("warning: failed to write failure_reason.txt: {e}");
        }
    }

    /// Get the directory path.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Get current status (for debugging/testing).
    pub fn status(&self) -> &AttemptStatus {
        &self.status
    }
}
