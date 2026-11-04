//! What a dry run of `fix` prints: what was read, which upstream fix was
//! found, and what would go into the package.

use crate::bugfix::{FixJob, FixedIn, Found};

fn first_line(text: &str) -> String {
    text.lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or_default()
        .trim()
        .to_string()
}

/// The lines of a dry run for `job` in the branch project `branch`, the package
/// at `version`; `origin` is where a request would go.
pub fn dry_run_lines(
    job: &FixJob,
    version: &str,
    branch: &str,
    origin: &str,
    submit: bool,
) -> Vec<String> {
    let mut out = vec![format!(
        "fix {} {version} in {branch} (the version stays {version}):",
        job.pkg
    )];
    for t in &job.bug_texts {
        let what = if t.source.starts_with("file ") {
            format!("given as text ({})", t.source)
        } else {
            format!(
                "read from {}, {} comment{} kept",
                t.source,
                t.comments.len(),
                if t.comments.len() == 1 { "" } else { "s" }
            )
        };
        let summary = if t.summary.is_empty() {
            first_line(t.comments.first().map_or("", String::as_str))
        } else {
            t.summary.clone()
        };
        out.push(format!("  report: \"{summary}\" ({what})"));
    }
    if job.description.is_some() {
        out.push("  report: given as text (--description)".to_string());
    }
    for n in &job.not_read {
        out.push(format!("  not read: {n}"));
    }
    out.push(format!(
        "  upstream repository: {}",
        job.upstream_repo.as_deref().unwrap_or("unknown")
    ));
    let how = match job.found {
        Found::Given => "(given)",
        Found::NamedInBug => "(named in the bug text)",
        Found::Search => "(GitHub commit search)",
        Found::NotFound => "",
    };
    match job.patches.as_slice() {
        [] if !job.candidates.is_empty() => {
            out.push(
                "  upstream fix: several candidates, none chosen (pass the ones to use with \
                 --commit-url):"
                    .to_string(),
            );
            out.extend(job.candidates.iter().map(|c| format!("     {c}")));
        }
        [] => out.push(
            "  upstream fix: none found: the agent looks in the repository, else writes the fix"
                .to_string(),
        ),
        patches => {
            for p in patches {
                let text = String::from_utf8_lossy(&p.bytes);
                out.push(format!("  upstream fix: {} {how}", p.origin));
                if job.unmerged.contains(&p.file_name) {
                    out.push(
                        "  warning: not shown to be merged upstream (a pull request, or a commit \
                         no release contains): flagged for a person's review"
                            .to_string(),
                    );
                }
                out.push(format!(
                    "  patch: {} ({} lines, sha256 {})",
                    p.file_name,
                    text.lines().count(),
                    sha12(&p.bytes)
                ));
                let header: Vec<&str> = text
                    .lines()
                    .filter(|l| l.starts_with("References:") || l.starts_with("Upstream:"))
                    .collect();
                out.push(format!("    header: {}", header.join(" / ")));
                out.push(format!(
                    "    spec:   {}",
                    crate::backport::spec_comment(p, &job.refs)
                ));
                out.push(format!(
                    "    .changes: {}",
                    crate::backport::changes_bullet(p, &job.refs)
                ));
            }
        }
    }
    out.push(format!(
        "  upstream release with the fix: {}",
        match &job.fixed_in {
            FixedIn::Tag(t) => format!(
                "{t} (`auto-package update --project <P> --only {} --to-version {}` would take it \
                 instead)",
                job.pkg,
                crate::bugfix::tag_version(t)
            ),
            FixedIn::NoneYet => "none yet".to_string(),
            FixedIn::NotChecked(why) => format!("not checked: {why}"),
        }
    ));
    out.push(if submit {
        format!("  submit: a request to {origin} after confirmation")
    } else {
        "  submit: no (--submit not given)".to_string()
    });
    out
}

fn sha12(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(bytes)
        .iter()
        .take(6)
        .map(|b| format!("{b:02x}"))
        .collect()
}
