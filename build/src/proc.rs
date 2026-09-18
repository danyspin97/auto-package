//! Subprocess execution with single-fd log capture and event emission.
//!
//! Both stdout and stderr are redirected to the same file descriptor so
//! the combined log is strictly chronological.

use std::collections::HashMap;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use ap_core::event::{Event, StepId};
use ap_core::sink::{NULL_REPORTER, Reporter};

#[derive(Debug, thiserror::Error)]
pub enum ProcessError {
    #[error("could not launch {program}: {source}")]
    Launch {
        program: String,
        #[source]
        source: io::Error,
    },
    #[error("io error: {0}")]
    Io(#[from] io::Error),
}

/// Where a subprocess reports the fact that it ran.
///
/// Carried explicitly rather than read from a global: the subprocess layer is
/// the one place where "what did this program actually do" is knowable, and
/// `--log debug` promises every argv and cwd (REDESIGN §3.4).
#[derive(Clone, Copy)]
pub struct Emit<'a> {
    pub reporter: &'a dyn Reporter,
    /// The numbered step this ran under, when there is one. Resolution and
    /// other pre-step work has none.
    pub step: Option<StepId>,
}

impl<'a> Emit<'a> {
    pub fn new(reporter: &'a dyn Reporter, step: StepId) -> Self {
        Self { reporter, step: Some(step) }
    }

    /// Report against the run rather than a numbered step.
    pub fn run(reporter: &'a dyn Reporter) -> Self {
        Self { reporter, step: None }
    }

    /// Record nothing. For calls whose individual invocations are noise —
    /// a status poll repeated every 15 seconds, say.
    pub fn quiet() -> Emit<'static> {
        Emit { reporter: &NULL_REPORTER, step: None }
    }

    fn started(&self, program: &str, argv: &[String], cwd: &Path, log: Option<&Path>) {
        self.reporter.report(&Event::Command {
            step: self.step,
            program: program.to_string(),
            argv: argv.to_vec(),
            cwd: cwd.to_path_buf(),
            log: log.map(Path::to_path_buf),
        });
    }

    fn done(&self, code: i32, elapsed: Duration, stdout_bytes: u64, stderr_bytes: u64) {
        self.reporter.report(&Event::CommandDone {
            step: self.step,
            code,
            elapsed,
            stdout_bytes,
            stderr_bytes,
        });
    }
}

pub struct ProcessOutput {
    pub code: i32,
    pub log_path: PathBuf,
    /// Total bytes written (stdout+stderr merged).
    pub stdout_bytes: u64,
    /// Always 0 — stderr is merged into stdout_bytes.
    pub stderr_bytes: u64,
    pub elapsed: Duration,
    /// Combined stdout+stderr content (same bytes as log_path).
    pub combined: Vec<u8>,
}

impl ProcessOutput {
    pub fn combined_str(&self) -> std::borrow::Cow<'_, str> {
        String::from_utf8_lossy(&self.combined)
    }

    pub fn success(&self) -> bool {
        self.code == 0
    }
}

/// Run a subprocess, capturing stdout+stderr to a single log file.
///
/// Both file descriptors share the same underlying file description so
/// writes are interleaved in arrival order. Emits `Event::Command` before
/// launch and `Event::CommandDone` after completion.
pub fn run(
    program: &str,
    args: &[impl AsRef<std::ffi::OsStr>],
    cwd: &Path,
    env: &HashMap<String, String>,
    log_path: &Path,
    emit: Emit<'_>,
) -> Result<ProcessOutput, ProcessError> {
    let argv: Vec<String> = args
        .iter()
        .map(|a| a.as_ref().to_string_lossy().into_owned())
        .collect();

    emit.started(program, &argv, cwd, Some(log_path));

    // Both stdout and stderr go to the same file; try_clone shares the offset.
    let log_file = File::create(log_path)?;
    let log_clone = log_file.try_clone()?;

    let start = Instant::now();
    let status = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .envs(env.iter())
        .stdin(Stdio::null())
        .stdout(Stdio::from(log_clone))
        .stderr(Stdio::from(log_file))
        .status()
        .map_err(|e| ProcessError::Launch {
            program: program.to_string(),
            source: e,
        })?;
    let elapsed = start.elapsed();

    let combined = std::fs::read(log_path).unwrap_or_default();
    let total_bytes = combined.len() as u64;
    let code = status.code().unwrap_or(-1);

    emit.done(code, elapsed, total_bytes, 0);

    Ok(ProcessOutput {
        code,
        log_path: log_path.to_path_buf(),
        stdout_bytes: total_bytes,
        stderr_bytes: 0,
        elapsed,
        combined,
    })
}

/// Run a subprocess and capture stdout/stderr separately as strings.
///
/// Intended for short commands (`osc ls`, `rpmspec --parse`) where the caller
/// needs to inspect the output. Emits the same `Command`/`CommandDone` pair as
/// [`run`], so `--log debug` shows every subprocess and not just the build.
pub fn capture(
    program: &str,
    args: &[impl AsRef<std::ffi::OsStr>],
    cwd: &Path,
    emit: Emit<'_>,
) -> Result<Captured, ProcessError> {
    let argv: Vec<String> = args
        .iter()
        .map(|a| a.as_ref().to_string_lossy().into_owned())
        .collect();

    emit.started(program, &argv, cwd, None);

    let start = Instant::now();
    let output = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| ProcessError::Launch {
            program: program.to_string(),
            source: e,
        })?;
    let elapsed = start.elapsed();

    let code = output.status.code().unwrap_or(-1);
    emit.done(
        code,
        elapsed,
        output.stdout.len() as u64,
        output.stderr.len() as u64,
    );

    Ok(Captured {
        code,
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

pub struct Captured {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Captured {
    pub fn success(&self) -> bool {
        self.code == 0
    }
}
