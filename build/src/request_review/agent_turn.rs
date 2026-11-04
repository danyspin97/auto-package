//! The optional agent turn of a review (`--agent`): one read-only model turn
//! over the files of the request, whose findings are labelled as opinion.
//!
//! The turn is [`agent::Task::RequestReview`]: it reads the workspace this
//! module writes and may fetch the upstream forge host of the source, only
//! when the allowed-hosts list already allows it. The deterministic report is
//! printed whatever the turn does, and the agent's findings never decide the
//! exit code unless `--fail-on-agent` is given.

use std::collections::HashSet;
use std::path::Path;

use crate::agent::{self, AgentFinding, Task, TurnOutcome, TurnRequest, untrusted_data};
use crate::allowed_hosts::{AllowedHosts, Use, normalize_host};
use crate::pipeline::{daily_cap_check, record_daily_spend, spend_allows_turn};
use crate::update::Ecosystem;

use super::report::{AGENT_NOTE, AgentSection};
use super::run::Reviewed;
use super::save::diff_name;
use super::{Finding, Origin, Severity, ids, plain_line};

/// The most findings of the agent that are kept.
pub const MAX_AGENT_FINDINGS: usize = 30;

/// The most bytes of the diff put in the prompt; the cut is said.
pub const MAX_DIFF_BYTES: usize = 200_000;

/// The most characters of a claim or its evidence that are kept.
const MAX_TEXT_CHARS: usize = 400;

/// What the turn needs besides the review.
pub struct AgentOptions<'a> {
    pub budget_usd: f64,
    /// The work directory: where the daily spend ledger is.
    pub work_dir: &'a Path,
    /// The review's directory; the turn works in `<dir>/agent/`.
    pub dir: &'a Path,
    /// Hosts the turn may fetch, already allowed by the allowed-hosts list.
    pub web_hosts: &'a [String],
}

/// Runs one turn, with the hosts it may fetch. Live: [`agent::turn_with_hosts`].
pub type RunTurn<'a> = dyn Fn(TurnRequest<'_>, &[String]) -> Result<TurnOutcome, agent::Error> + 'a;

/// What must hold before the review starts, so that a review that cannot run
/// its turn fails at once, with no report: the CLI is logged in and the day's
/// spend leaves room. `login` and `cap` are [`agent::ensure_logged_in`] and
/// [`daily_cap_check`] when live.
pub fn preflight(
    work_dir: &Path,
    login: &dyn Fn() -> Result<(), String>,
    cap: &dyn Fn(&Path) -> Result<(), String>,
) -> Result<(), String> {
    login()?;
    cap(work_dir)
}

/// The live preflight.
pub fn live_preflight(work_dir: &Path) -> Result<(), String> {
    preflight(work_dir, &agent::ensure_logged_in, &daily_cap_check)
}

/// A file name that is safe to write into the workspace: one plain name.
fn safe_name(name: &str) -> bool {
    !name.is_empty() && !name.starts_with('.') && !name.contains(['/', '\0', '\\'])
}

/// Write what the turn reads into `ws`: the request, the diff, both sides'
/// files and the checks' findings.
pub fn prepare_workspace(reviewed: &Reviewed, ws: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(ws)?;
    std::fs::write(ws.join("request.xml"), &reviewed.inputs.request_xml)?;
    let count = reviewed.inputs.actions.len();
    for (i, a) in reviewed.inputs.actions.iter().enumerate() {
        let own = |base: &str| {
            // The index keeps two actions on one package name apart.
            let d = if count == 1 {
                ws.join(base)
            } else {
                ws.join(base).join(format!("{i}-{}", a.target.package))
            };
            std::fs::create_dir_all(&d).map(|()| d)
        };
        for (base, side) in [("old", &a.old), ("new", &a.new)] {
            let dir = own(base)?;
            for (name, text) in side.texts.iter().filter(|(n, _)| safe_name(n)) {
                std::fs::write(dir.join(name), text)?;
            }
        }
        if let Some(diff) = &a.diff {
            let name = diff_name(count, i, &a.target.package);
            if safe_name(&name) {
                std::fs::write(ws.join(name), diff)?;
            }
        }
    }
    let found: Vec<&Finding> = reviewed.report.check_findings().collect();
    let json = serde_json::to_string_pretty(&found).map_err(std::io::Error::other)?;
    std::fs::write(ws.join("findings.json"), json + "\n")
}

/// `text` cut to at most `max` bytes at a character boundary.
fn cut(text: &str, max: usize) -> &str {
    let mut end = text.len().min(max);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// The Context of the turn. Everything that came from the request or the
/// package (its creator, the versions, the checks' messages, the description,
/// the diff) is inside an untrusted-data block: only the fixed words and the
/// hosts the turn may fetch are outside one.
pub fn context(reviewed: &Reviewed, web_hosts: &[String]) -> String {
    let r = &reviewed.report;
    let mut header = format!(
        "Request {} ({}, by {}).\n",
        plain_line(&r.request.id),
        plain_line(&r.request.state),
        plain_line(&r.request.creator)
    );
    for a in &r.actions {
        header.push_str(&format!(
            "- {}/{}@{} -> {}/{}: version {} -> {}\n",
            plain_line(&a.source.project),
            plain_line(&a.source.package),
            plain_line(a.source.rev.as_deref().unwrap_or("current")),
            plain_line(&a.target.project),
            plain_line(&a.target.package),
            plain_line(a.old_version.as_deref().unwrap_or("none")),
            plain_line(a.new_version.as_deref().unwrap_or("unknown")),
        ));
    }
    let mut out = format!("{}\n", untrusted_data("request header", &header));
    let mut findings = String::new();
    for f in r.check_findings() {
        findings.push_str(&format!(
            "- {} [{}] {}\n",
            f.id,
            f.severity.word(),
            f.message
        ));
    }
    if findings.is_empty() {
        findings.push_str("- none\n");
    }
    out.push_str(&format!(
        "\nFindings of the deterministic checks (do not repeat them; their text comes from the package):\n{}\n",
        untrusted_data("check findings", &findings)
    ));
    out.push_str(&format!(
        "\n{}\n",
        untrusted_data("request description", &r.request.description)
    ));
    if web_hosts.is_empty() {
        out.push_str("\nYou may fetch no web page.\n");
    } else {
        out.push_str(&format!(
            "\nYou may fetch pages of these hosts only (WebFetch): {}.\n",
            web_hosts.join(", ")
        ));
    }
    for a in &reviewed.inputs.actions {
        let Some(diff) = &a.diff else {
            out.push_str(&format!(
                "\nThe diff of {} could not be read: compare old/ and new/.\n",
                plain_line(&a.target.package)
            ));
            continue;
        };
        let shown = cut(diff, MAX_DIFF_BYTES);
        let note = if shown.len() < diff.len() {
            format!(
                " (cut at {MAX_DIFF_BYTES} of {} bytes; the whole diff is in the workspace)",
                diff.len()
            )
        } else {
            String::new()
        };
        out.push_str(&format!(
            "\nThe source diff of {}{note}:\n{}\n",
            plain_line(&a.target.package),
            untrusted_data("source diff", shown)
        ));
    }
    out
}

/// The hosts the turn may fetch: the upstream forge of each action's spec
/// (what `update` looks releases up on), when `hosts` allows it for a source
/// fetch AND the list names exactly that host. A user entry also covers its
/// subdomains, but a spec from a request does not get to pick one of them for
/// the grant. The agent cannot allow a host; only the list can.
pub fn forge_hosts(reviewed: &Reviewed, hosts: &AllowedHosts) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for a in &reviewed.inputs.actions {
        let Some(header) = a.new.spec().and_then(crate::spec::parse_header) else {
            continue;
        };
        for e in crate::update::ecosystems(&header, None) {
            let Ecosystem::GitRepo { url } = e else {
                continue;
            };
            let authority = url.split_once("://").map_or(url.as_str(), |(_, r)| r);
            let host = authority.split('/').next().unwrap_or("");
            let host = host.rsplit('@').next().unwrap_or(host);
            let host = host.split(':').next().unwrap_or(host);
            if let Ok(host) = normalize_host(host)
                && hosts.allows(&host, Use::Source)
                && hosts.entries().iter().any(|e| e.host == host)
                && !out.contains(&host)
            {
                out.push(host);
            }
        }
    }
    out
}

/// `text` as one plain line of at most [`MAX_TEXT_CHARS`] characters.
fn clean(text: &str) -> String {
    let line = plain_line(text);
    if line.chars().count() > MAX_TEXT_CHARS {
        let head: String = line.chars().take(MAX_TEXT_CHARS).collect();
        format!("{}...", head.trim_end())
    } else {
        line
    }
}

fn severity_of(word: &str) -> Severity {
    match word.trim().to_ascii_lowercase().as_str() {
        "blocker" => Severity::Blocker,
        "warning" => Severity::Warning,
        _ => Severity::Note,
    }
}

/// The agent's findings as the report shows them: at most
/// [`MAX_AGENT_FINDINGS`], each claim and evidence cleaned. A finding whose file
/// is not one of `files` says so and is kept. A claim that starts with a check's
/// id and a colon is a dispute of that check.
pub fn map_findings(raw: &[AgentFinding], package: &str, files: &HashSet<String>) -> Vec<Finding> {
    raw.iter()
        .take(MAX_AGENT_FINDINGS)
        .map(|f| {
            let claim = clean(&f.claim);
            let disputed = ids::ALL.iter().any(|id| {
                claim
                    .strip_prefix(id)
                    .is_some_and(|rest| rest.starts_with(':'))
            });
            let file = clean(&f.file);
            let known = files.contains(&file)
                || file == "request.xml"
                || files.iter().any(|n| file.ends_with(&format!("/{n}")));
            let outside = if known {
                ""
            } else {
                " (file not in the request)"
            };
            let mut finding = Finding {
                id: if disputed {
                    ids::AGENT_DISPUTE
                } else {
                    ids::AGENT_FINDING
                },
                severity: severity_of(&f.severity),
                origin: Origin::Agent,
                package: package.to_string(),
                file: Some(file),
                line: f.line,
                message: String::new(),
            };
            finding.message = crate::fix_flags::one_line(&format!(
                "{claim} (evidence: {}){outside}",
                clean(&f.evidence)
            ));
            finding
        })
        .collect()
}

/// The prompt, the answer and the tool calls of the turn, saved like a run's.
fn save_turn(dir: &Path, prompt: Option<&str>, raw: &str, output: &str) {
    let _ = std::fs::create_dir_all(dir);
    if let Some(p) = prompt {
        let _ = std::fs::write(dir.join("prompt.md"), p);
    }
    let _ = std::fs::write(dir.join("response.json"), raw);
    let _ = std::fs::write(dir.join("output.md"), output);
}

fn failed(error: String) -> AgentSection {
    AgentSection {
        ran: false,
        model: None,
        cost_usd: None,
        error: Some(error),
        note: AGENT_NOTE,
        findings: Vec::new(),
    }
}

/// Run the turn and give its section. A failure is in the section, not an
/// error: the deterministic report stands.
pub fn agent_review(reviewed: &Reviewed, o: &AgentOptions<'_>, run: &RunTurn<'_>) -> AgentSection {
    let ws = o.dir.join("agent");
    if let Err(e) = prepare_workspace(reviewed, &ws) {
        return failed(format!("cannot prepare {}: {e}", ws.display()));
    }
    let context = context(reviewed, o.web_hosts);
    let first = reviewed.inputs.actions.first();
    let package = first.map_or("", |a| a.target.package.as_str());
    let version = reviewed
        .report
        .actions
        .first()
        .and_then(|a| a.new_version.as_deref())
        .unwrap_or("");
    let mut spent = 0.0;
    let mut rotation = 0;
    loop {
        let request = TurnRequest {
            workspace: &ws,
            pkg_name: package,
            version,
            is_python: false,
            task: Task::RequestReview,
            attempt: 1,
            max_attempts: 1,
            spent_usd: spent,
            budget_usd: o.budget_usd,
            hint: None,
            notes: "",
            package_notes: "",
            from_repo_archive: false,
            subprojects: &[],
            unresolvable_deps: &[],
            unresolvable_detail: &[],
            failed_deps: &[],
            unbuildable_deps: &[],
            session_id: None,
            checkout_dir: None,
            context: Some(&context),
            carried: false,
            rpms_dir: None,
            sibling_dirs: &[],
            model_rotation: rotation,
            writable_patches: &[],
            fetch_hosts: &[],
            on_tool_use: None,
        };
        let model = agent::model_for_task(Task::RequestReview, 1, rotation).to_string();
        match run(request, o.web_hosts) {
            Ok(out) => {
                let cost = out.usage.cost_usd();
                record_daily_spend(o.work_dir, cost);
                save_turn(&ws, Some(&out.prompt), &out.raw, &out.output);
                let files: HashSet<String> = reviewed
                    .inputs
                    .actions
                    .iter()
                    .flat_map(|a| a.old.texts.keys().chain(a.new.texts.keys()))
                    .cloned()
                    .collect();
                let mut findings = map_findings(&out.review_findings, package, &files);
                if out.action != agent::Action::NoChange {
                    findings.push(Finding {
                        id: ids::AGENT_BAD_ANSWER,
                        severity: Severity::Note,
                        origin: Origin::Agent,
                        package: package.to_string(),
                        file: None,
                        line: None,
                        message:
                            "the agent answered with an action other than no_change; it was ignored"
                                .into(),
                    });
                }
                return AgentSection {
                    ran: true,
                    model: Some(model),
                    cost_usd: Some(spent + cost),
                    error: None,
                    note: AGENT_NOTE,
                    findings,
                };
            }
            Err(e) => {
                let cost = e.usage().map_or(0.0, |u| u.cost_usd());
                record_daily_spend(o.work_dir, cost);
                spent += cost;
                save_turn(&ws, e.prompt(), e.stdout().unwrap_or(""), &format!("{e}\n"));
                let again = e.is_rate_limited()
                    && rotation == 0
                    && spend_allows_turn(o.budget_usd, spent, cost).is_ok();
                if again {
                    rotation = 1;
                    continue;
                }
                return failed(e.to_string());
            }
        }
    }
}
