use super::*;

pub fn update_after_build(
    ws: &Workspace,
    rpm_name: &str,
    version: &str,
    project: &str,
    distro: &str,
    arch: &str,
) -> NotesRun {
    let mut usage = Usage::default();
    let result = update_inner(ws, rpm_name, version, project, distro, arch, &mut usage);
    NotesRun { usage, result }
}

pub(super) fn update_inner(
    ws: &Workspace,
    rpm_name: &str,
    version: &str,
    project: &str,
    distro: &str,
    arch: &str,
    usage: &mut Usage,
) -> Result<(), String> {
    let Some(notes_file) = path_for(rpm_name) else {
        return Ok(());
    };
    if let Some(dir) = notes_file.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }
    // Held across read, agent and write: a second build for the same package
    // waits here instead of overwriting this build's update.
    let _lock = NotesLock::acquire(&notes_file)?;
    // Missing is the normal first-build case; any other read error means the
    // existing notes would be overwritten from scratch, so stop instead.
    let before = read_or_empty(&notes_file)
        .map_err(|e| format!("cannot read {}: {e}", notes_file.display()))?;
    // The agent edits a private copy and the result is renamed into place, so
    // a failed or concurrent build never leaves the shared notes half-written.
    let dir = notes_file.parent().unwrap_or_else(|| Path::new("."));
    // The lock is ours, so no live update for this package owns a scratch file.
    remove_stale_scratch(dir, rpm_name);
    // Not `.md`: a scratch file left behind by an interrupted run must not look
    // like a package's notes.
    let prefix = scratch_prefix(rpm_name);
    let scratch = tempfile::Builder::new()
        .prefix(&prefix)
        .suffix(".tmp")
        .tempfile_in(dir)
        .map_err(|e| {
            format!(
                "cannot create a scratch notes file in {}: {e}",
                dir.display()
            )
        })?;
    std::fs::write(scratch.path(), &before)
        .map_err(|e| format!("cannot seed {}: {e}", scratch.path().display()))?;
    // The agent may only read and write its scratch file (the journal is under
    // the working directory, which is readable anyway); other packages' notes
    // stay out of reach.
    // `//` marks an absolute path in a permission rule.
    let abs = |p: &Path| {
        let p = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
        format!("/{}", p.display())
    };
    let tools = format!(
        "Read({scratch}),Write({scratch}),Edit({scratch})",
        scratch = abs(scratch.path()),
    );
    // Path rules in `--allowed-tools` are only enforced in the default
    // permission mode with project-only settings; user settings could
    // otherwise widen what the agent may write.
    // The newest journal entries are inlined; older ones stay in the file.
    // The journal is only read, never rewritten, so an unreadable one costs
    // the prompt its context but cannot lose anything; say so.
    let journal = read_or_empty(&ws.notes()).unwrap_or_else(|e| {
        ap_core::diag!(
            "  cannot read {}: {e}; notes prompt has no journal",
            ws.notes().display()
        );
        String::new()
    });
    let journal_tail = crate::pipeline::tail_for_prompt(&journal);
    // One turn under `budget` dollars; returns what it cost. The cost is also
    // added to `usage`, whether or not the turn worked.
    let mut run_turn = |prompt: &str, budget: f64| -> Result<f64, String> {
        let out = claude_print_json(
            NOTES_MODEL,
            &tools,
            &format!("{budget:.2}"),
            prompt,
            ws.dir(),
            NOTES_TIMEOUT,
        )
        .map_err(|e| match e {
            // Nothing ran, so nothing was spent.
            crate::proc::ProcessError::Launch { .. } => format!("could not run claude: {e}"),
            // The process ran and its reply, with the real cost, was lost. The
            // turn can have spent up to its cap, so charge that as an upper bound
            // rather than let a timed-out turn read as free.
            _ => {
                usage.cost_usd += budget;
                format!(
                    "could not run claude: {e}; cost counted as the ${budget:.2} \
                     turn cap, an estimate"
                )
            }
        })?;
        let turn = usage_from_output(&out.stdout, budget);
        let mut cost = turn.cost_usd;
        // A nonzero exit with no usage report (killed, or crashed before printing)
        // may still have spent money: charge the cap as an upper bound.
        if out.code != 0 && cost == 0.0 {
            cost = budget;
        }
        usage.input_tokens += turn.input_tokens;
        usage.output_tokens += turn.output_tokens;
        usage.cost_usd += cost;
        if out.code != 0 {
            return Err(format!(
                "claude exited with {}: {}",
                out.code,
                out.stderr.trim()
            ));
        }
        Ok(cost)
    };
    let prompt_for = |feedback: Option<&str>| {
        notes_prompt(
            rpm_name,
            version,
            project,
            distro,
            arch,
            &ws.notes(),
            scratch.path(),
            &journal_tail,
            before.lines().count(),
            feedback,
        )
    };
    let mut budget_left = NOTES_CAP_USD;
    let mut feedback: Option<String> = None;
    let after = loop {
        budget_left =
            (budget_left - run_turn(&prompt_for(feedback.as_deref()), budget_left)?).max(0.0);
        // A notes file left by an earlier build is not an update from this one.
        let after = std::fs::read_to_string(scratch.path())
            .map_err(|e| format!("cannot read {}: {e}", scratch.path().display()))?;
        if after.trim().is_empty() || after == before {
            // A retry that undoes the refused text leaves the notes as they
            // were; the refusal is the reason, not "did not update".
            if let Some(reason) = &feedback {
                return Err(format!("{reason}; kept the previous notes"));
            }
            return Err(format!("the agent did not update {}", notes_file.display()));
        }
        match check_size(&after).and_then(|()| check_injection(&before, &after)) {
            Ok(()) => break after,
            // One retry with the reason fed back, if the budget still allows it.
            Err(e) if feedback.is_none() && budget_left >= MIN_RETRY_USD => {
                ap_core::diag!("  notes update refused ({e}); retrying once");
                feedback = Some(e);
            }
            Err(e) => return Err(format!("{e}; kept the previous notes")),
        }
    };
    ap_core::diag!("  notes for {rpm_name}: {}", diff_summary(&before, &after));
    ap_core::fsx::write_atomic(&notes_file, &after)
        .map_err(|e| format!("cannot write {}: {e}", notes_file.display()))?;
    // The notes are already saved; a failed commit must not fail the update.
    if let Err(e) = commit_notes(dir, &notes_file, rpm_name, version) {
        ap_core::diag!("  notes for {rpm_name} saved but not committed: {e}");
    }
    Ok(())
}

/// Prefix of one package's scratch files. `sanitize_name` never keeps a `~`, so
/// no package's prefix can match another package's files.
pub(super) fn scratch_prefix(rpm_name: &str) -> String {
    format!(".scratch-{rpm_name}~")
}

/// Delete the scratch files an interrupted update left for `rpm_name`. Only
/// called while holding that package's lock.
pub(super) fn remove_stale_scratch(dir: &Path, rpm_name: &str) {
    let prefix = scratch_prefix(rpm_name);
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if name
            .to_str()
            .is_some_and(|n| n.starts_with(&prefix) && n.ends_with(".tmp"))
        {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Refuse a notes file over the size cap, so one turn cannot grow the notes
/// without bound.
pub(super) fn check_size(notes: &str) -> Result<(), String> {
    let lines = notes.lines().count();
    if lines > MAX_NOTES_LINES || notes.len() > MAX_NOTES_BYTES {
        return Err(format!(
            "the notes update is {lines} lines and {} bytes, over the \
             {MAX_NOTES_LINES}-line, {MAX_NOTES_BYTES}-byte cap",
            notes.len()
        ));
    }
    Ok(())
}

/// Text that turns notes into an instruction channel: the build agent reads
/// them as memory, so a URL or a destructive or fetching command that was not
/// already in the notes is refused. Conservative on purpose; a refused update
/// is retried once and otherwise leaves the previous notes in place.
pub(super) const SUSPICIOUS: &[&str] = &[
    "http://",
    "https://",
    "ftp://",
    "ssh://",
    "git@",
    "git clone ",
    "curl ",
    "wget ",
    "rm -rf",
    "rm -fr",
    "sudo ",
    "osc ",
    "| sh",
    "| bash",
    "bash -c",
    "sh -c",
    "eval ",
    "chmod ",
    "base64 ",
    "python -c",
    "python3 -c",
    "perl -e",
    "ruby -e",
    "nc ",
    // Imperatives aimed at the model that reads the notes, and the spec parts
    // a hostile note would want a later turn to write.
    "ignore previous",
    "system prompt",
    "disregard",
    "you must now",
    "new instructions",
    "source99",
    "scriptlet",
    "%post",
    "%pre ",
];

/// Whether `pat` occurs at byte `i` of `hay` as a whole token: a pattern that
/// starts or ends with a word character must not touch another one, so `curl `
/// is not found in `libcurl ` or `pycurl `, nor `eval ` in `retrieval `.
pub(super) fn whole_token_at(hay: &str, i: usize, pat: &str) -> bool {
    let word = |c: char| c.is_alphanumeric() || c == '_' || c == '-';
    let (first, last) = (pat.chars().next(), pat.chars().last());
    let before_ok = !first.is_some_and(word) || !hay[..i].chars().next_back().is_some_and(word);
    let after_ok =
        !last.is_some_and(word) || !hay[i + pat.len()..].chars().next().is_some_and(word);
    before_ok && after_ok
}

/// The URLs and commands in `text` (already lowercased), each as the exact span
/// the notes carry: a URL runs to the next whitespace, a command to the end of
/// its line, so `curl https://a.example` is one span with its argument. Two
/// texts that carry the same span carry the same URL or command.
pub(super) fn suspicious_spans(text: &str) -> std::collections::BTreeSet<String> {
    let mut spans = std::collections::BTreeSet::new();
    for pat in SUSPICIOUS {
        for (i, _) in text.match_indices(pat) {
            if !whole_token_at(text, i, pat) {
                continue;
            }
            let rest = &text[i..];
            let end = if pat.ends_with("://") {
                rest.find(char::is_whitespace).unwrap_or(rest.len())
            } else {
                rest.find('\n').unwrap_or(rest.len())
            };
            spans.insert(rest[..end].trim_end().to_string());
        }
    }
    spans
}

/// Refuse an update that adds a URL or command span the old notes do not carry
/// verbatim. A span present in the old notes can be restated; a URL that merely
/// starts like one in the old notes (`https://a.example/x` after
/// `https://a.example`) is a new span and is refused.
pub(super) fn check_injection(before: &str, after: &str) -> Result<(), String> {
    let old = suspicious_spans(&before.to_lowercase());
    let new = suspicious_spans(&after.to_lowercase());
    if let Some(span) = new.difference(&old).next() {
        return Err(format!(
            "the notes update adds `{span}`, which notes may not contain (no URLs or \
             shell commands not already in the notes)"
        ));
    }
    Ok(())
}

/// One line saying how much an update changed, for the run output.
pub(super) fn diff_summary(before: &str, after: &str) -> String {
    let old: std::collections::HashSet<&str> = before.lines().collect();
    let new: std::collections::HashSet<&str> = after.lines().collect();
    let added = after.lines().filter(|l| !old.contains(l)).count();
    let removed = before.lines().filter(|l| !new.contains(l)).count();
    format!(
        "updated, +{added}/-{removed} lines ({} -> {} lines)",
        before.lines().count(),
        after.lines().count()
    )
}

/// The commit message for an updated notes file.
pub(super) fn commit_message(rpm_name: &str, version: &str) -> String {
    format!("notes: {rpm_name} {version}")
}

/// Commit `file` when `dir` is a git repository of its own, so the notes can
/// be shared and versioned as a git module. Never pushes. The repository is
/// the user's to create: without a `.git` entry (a directory, or the file a
/// submodule or worktree has) directly in `dir` this does nothing. A notes
/// directory that merely sits inside another repository, such as the project's
/// own checkout, is deliberately left alone rather than adding commits there.
///
/// Only `file` is committed (`commit -- <file>`), so if `git add` or `git
/// commit` fails, whatever else is staged in that repository is never swept
/// into a later notes commit; the file stays staged and the next update retries.
pub(super) fn commit_notes(
    dir: &Path,
    file: &Path,
    rpm_name: &str,
    version: &str,
) -> Result<(), String> {
    if !dir.join(".git").exists() {
        // Said once per run, not per package: notes only being saved locally
        // is a setup choice, but an unnoticed one loses the sharing.
        static WARNED: std::sync::Once = std::sync::Once::new();
        WARNED.call_once(|| {
            ap_core::diag!(
                "  note: {} is not a git repository, so package notes are saved but not committed",
                dir.display()
            );
        });
        return Ok(());
    }
    let git = |args: &[&str]| {
        let mut full: Vec<String> = vec!["-C".into(), dir.display().to_string()];
        full.extend(args.iter().map(|a| a.to_string()));
        crate::proc::capture_with_timeout_env(
            "git",
            &full,
            dir,
            crate::proc::Emit::quiet(),
            GIT_TIMEOUT,
            None,
        )
        .map_err(|e| format!("git {}: {e}", args.first().unwrap_or(&"")))
    };
    let name = file
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("notes file name is not UTF-8")?;
    let add = git(&["add", "--", name])?;
    if add.code != 0 {
        return Err(format!("git add failed: {}", add.stderr.trim()));
    }
    // Nothing to commit when the file already matches HEAD.
    let status = git(&["status", "--porcelain", "--", name])?;
    if status.code == 0 && status.stdout.trim().is_empty() {
        return Ok(());
    }
    let message = commit_message(rpm_name, version);
    // The environment's identity wins; the placeholder only keeps the commit
    // from failing where git has no identity configured.
    let has_identity = std::env::var_os("GIT_AUTHOR_EMAIL").is_some()
        || git(&["config", "user.email"]).is_ok_and(|o| !o.stdout.trim().is_empty());
    let mut args = Vec::new();
    if !has_identity {
        args.extend([
            "-c",
            "user.name=auto-package",
            "-c",
            "user.email=auto-package@localhost",
        ]);
    }
    args.extend(["commit", "-m", &message, "--", name]);
    let commit = git(&args)?;
    if commit.code != 0 {
        return Err(format!("git commit failed: {}", commit.stderr.trim()));
    }
    Ok(())
}

/// Token counts and cost from `claude --output-format json`. A reply that does
/// not parse does not fail the notes update, but the turn may have spent up to
/// its cap, so that is charged (with a warning) rather than nothing.
pub(super) fn usage_from_output(stdout: &str, cap_usd: f64) -> Usage {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(stdout) else {
        ap_core::diag!(
            "  notes reply was not JSON; cost counted as the ${cap_usd:.2} turn cap, an estimate"
        );
        return Usage {
            cost_usd: cap_usd,
            ..Usage::default()
        };
    };
    Usage {
        input_tokens: v["usage"]["input_tokens"].as_u64().unwrap_or(0),
        output_tokens: v["usage"]["output_tokens"].as_u64().unwrap_or(0),
        cost_usd: v["total_cost_usd"].as_f64().unwrap_or(0.0),
    }
}

/// Read a notes or journal file, treating a missing file as empty. Any other
/// error is returned: an unreadable file must not look like "no notes", or the
/// next update would overwrite it from scratch.
pub fn read_or_empty(path: &Path) -> std::io::Result<String> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(e),
    }
}

/// The package's notes as they stand: `Ok("")` when there are none yet (or no
/// notes directory is set), an error when the file exists but cannot be read.
pub fn read_checked(rpm_name: &str) -> std::io::Result<String> {
    match path_for(rpm_name) {
        Some(p) => read_or_empty(&p),
        None => Ok(String::new()),
    }
}

/// [`read_checked`] for prompt building: a read error is reported on the
/// diagnostic stream and the prompt goes without notes, never silently.
pub fn read(rpm_name: &str) -> String {
    read_checked(rpm_name).unwrap_or_else(|e| {
        ap_core::diag!("  cannot read package notes for {rpm_name}: {e}");
        String::new()
    })
}

#[allow(clippy::too_many_arguments)]
pub(super) fn notes_prompt(
    rpm_name: &str,
    version: &str,
    project: &str,
    distro: &str,
    arch: &str,
    journal: &Path,
    notes_file: &Path,
    journal_tail: &str,
    current_lines: usize,
    refused: Option<&str>,
) -> String {
    let refusal = refused.map_or_else(String::new, |why| {
        format!(
            "Your previous update was refused: {why}. Fix `{}` in place so it \
             passes; the limits are hard.\n\n",
            notes_file.display()
        )
    });
    format!(
        "You are keeping the notes for the openSUSE package {rpm_name} \
         (version {version}), which just built successfully in project \
         {project} for {distro}.\n\n\
         The newest part of the attempt journal is at the end of this prompt: \
         it is the build agent's own record of this package. Older entries are in \
         `{journal}` if you need them. Read `{notes_file}` too if it exists: it is \
         this package's memory from earlier builds. Then update `{notes_file}` in place \
         (create it if missing) as a short Markdown document with these sections, \
         omitting any that are empty. Keep what is still true, correct what this \
         build changed, and drop what no longer applies:\n\n\
         - **Build requirements**: BuildRequires and Requires that matter, and \
           why any non-obvious one is there.\n\
         - **Test changes**: tests deselected, ignored or patched, and why.\n\
         - **Workarounds**: patches, source fetches, environment fixes, and \
           the reason for each.\n\
         - **Failure history**: each failure that was fixed, one line each, \
           with the fix.\n\n\
         The notes are shared by every project, distro and architecture this \
         package is built for, so write each finding to hold on all of them. \
         Describe the rule, not one architecture's file: say \"the upstream wheel \
         for the build architecture\", not a wheel name or hash for x86_64. When a \
         finding only applies to one target, label it (for example \"on \
         openSUSE:Leap:16.0 only\" or \"on x86_64 only\"); leave general findings \
         unlabelled. This build is {project} / {distro} / {arch}; findings from \
         earlier builds that name a different target are not evidence about it.\n\n\
         Under each section you change, add a line saying which build it was \
         checked against, for example `Checked against {version} on {distro}, \
         {arch}.`, so a later build can judge how old the entry is.\n\n\
         The notes file has {current_lines} lines now. Keep it factual and at most \
         {TARGET_NOTES_LINES} lines (the hard limit is {MAX_NOTES_LINES} lines and \
         {MAX_NOTES_BYTES} bytes): merge and drop old entries to make room, and drop \
         entries whose `Checked against` version is much older than {version} unless \
         this build confirms them. Do not copy the journal verbatim. Do not add URLs \
         or shell commands; describe the fix in words. Do not touch any other file, \
         and do not run commands.\n\n\
         {refusal}{journal_block}",
        journal = journal.display(),
        notes_file = notes_file.display(),
        journal_block = crate::agent::untrusted_data("journal", journal_tail),
    )
}
