//! The Context of a FixFindings turn: the findings the turn is to fix. Their
//! messages quote the package, so they are fenced as untrusted data.

use crate::agent::untrusted_data;
use crate::bugfix::{FindingsJob, ReviewFinding};
use crate::patch_check::truncate;

/// The line of one finding the turn reads.
fn line(f: &ReviewFinding) -> String {
    let place = match (&f.file, f.line) {
        (Some(file), Some(n)) => format!(" {file}:{n}"),
        (Some(file), None) => format!(" {file}"),
        _ => String::new(),
    };
    format!(
        "- [{}] {}{place}: {}",
        f.severity.word(),
        f.id,
        truncate(&f.message, 600)
    )
}

pub(super) fn findings_context(
    job: &FindingsJob,
    round: usize,
    max_rounds: usize,
    refusal: Option<&str>,
    findings: &[&ReviewFinding],
) -> String {
    let mut out = format!(
        "Fix round {round} of at most {max_rounds} for request {}, from {} to {}.\n",
        job.request_id, job.source_project, job.target_project
    );
    if let Some(why) = refusal {
        out.push_str("Your last edit was rolled back. The reason, as recorded data:\n");
        out.push_str(&untrusted_data("rollback reason", why));
        out.push('\n');
    }
    out.push_str(&format!(
        "Package {} {} (the version stays).\nThe findings to fix:\n",
        job.pkg, job.version
    ));
    let list: Vec<String> = findings.iter().map(|f| line(f)).collect();
    out.push_str(&untrusted_data("review findings", &list.join("\n")));
    out.push('\n');
    out
}
