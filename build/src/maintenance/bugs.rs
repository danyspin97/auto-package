use super::*;

// ---------------------------------------------------------------------------
// Bug references
// ---------------------------------------------------------------------------

pub(super) static BUG_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"(?i)^(bsc|boo|bnc)#([0-9]{4,8})$").expect("valid regex"));

pub(super) static JIRA_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"(?i)^jsc#([a-z][a-z0-9]*)-([0-9]+)$").expect("valid regex")
});

pub(super) static CVE_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"(?i)^cve-((?:19|20)[0-9]{2})-([0-9]{4,7})$").expect("valid regex")
});

/// A bug reference in the spelling SUSE changelogs use (`bsc#1234567`,
/// `jsc#SLE-12345`, `CVE-2025-1234`), or why `s` is not one.
pub fn parse_bug_ref(s: &str) -> Result<String, String> {
    let s = s.trim();
    if let Some(c) = BUG_RE.captures(s) {
        return Ok(format!("{}#{}", c[1].to_lowercase(), &c[2]));
    }
    if let Some(c) = JIRA_RE.captures(s) {
        return Ok(format!("jsc#{}-{}", c[1].to_uppercase(), &c[2]));
    }
    if let Some(c) = CVE_RE.captures(s) {
        return Ok(format!("CVE-{}-{}", &c[1], &c[2]));
    }
    Err(format!(
        "{s:?} is not a bug reference: use bsc#NNNNNNN (also boo#, bnc#), jsc#PROJECT-NNN or CVE-YYYY-NNNN"
    ))
}

/// Whether a reference names a CVE.
pub fn is_cve(r: &str) -> bool {
    r.starts_with("CVE-")
}

/// The references of `--bug`, checked and in SUSE spelling, each once.
/// Without any, a maintenance request is refused unless `no_bug_ref`.
pub fn parse_bug_refs(given: &[String], no_bug_ref: bool) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::new();
    for g in given
        .iter()
        .flat_map(|g| g.split(','))
        .map(str::trim)
        .filter(|g| !g.is_empty())
    {
        let r = parse_bug_ref(g)?;
        if !out.contains(&r) {
            out.push(r);
        }
    }
    if out.is_empty() && !no_bug_ref {
        return Err(
            "a maintenance update needs a bug reference: pass --bug bsc#NNNNNNN (and --bug CVE-YYYY-NNNN for a \
             security fix), or --no-bug-ref to submit without one"
                .to_string(),
        );
    }
    Ok(out)
}

/// What to say about a set of references that is valid but unusual: a CVE
/// without the bsc# bug that tracks it.
pub fn bug_ref_warnings(refs: &[String]) -> Vec<String> {
    let has_bug = refs
        .iter()
        .any(|r| r.starts_with("bsc#") || r.starts_with("boo#") || r.starts_with("bnc#"));
    if refs.iter().any(|r| is_cve(r)) && !has_bug {
        vec![
            "a CVE is fixed but no bsc# bug is given; SUSE tracks every CVE with a bsc# bug"
                .to_string(),
        ]
    } else {
        Vec::new()
    }
}

/// `(bsc#1, CVE-2025-1)`.
pub fn refs_suffix(refs: &[String]) -> String {
    format!("({})", refs.join(", "))
}

/// Whether `text` mentions `r` as a whole reference (`bsc#12` is not in `bsc#123`).
pub(super) fn mentions(text: &str, r: &str) -> bool {
    let lower = text.to_lowercase();
    let r = r.to_lowercase();
    let mut from = 0;
    while let Some(i) = lower[from..].find(&r) {
        let end = from + i + r.len();
        let before_ok = from + i == 0 || !lower.as_bytes()[from + i - 1].is_ascii_alphanumeric();
        let after_ok = lower
            .as_bytes()
            .get(end)
            .is_none_or(|b| !b.is_ascii_alphanumeric());
        if before_ok && after_ok {
            return true;
        }
        from = end;
    }
    false
}

/// The references of `refs` that `text` does not mention yet.
pub fn missing_refs(text: &str, refs: &[String]) -> Vec<String> {
    refs.iter()
        .filter(|r| !mentions(text, r))
        .cloned()
        .collect()
}

pub(super) fn is_separator(l: &str) -> bool {
    let l = l.trim();
    l.len() >= 20 && l.bytes().all(|b| b == b'-')
}

/// `changes` with the references its newest entry lacks added to the end of
/// that entry's first bullet, before a closing `:`, the way SUSE writes them:
/// `- Update to 1.2.3 (bsc#123456, CVE-2025-1234):`. An entry with no bullet
/// gets a `- References: ...` one. `None` when the entry already names them
/// all, or there is no entry.
pub fn with_refs_in_newest_entry(changes: &str, refs: &[String]) -> Option<String> {
    let lines: Vec<&str> = changes.lines().collect();
    let first = lines.iter().position(|l| is_separator(l))?;
    let end = lines[first + 1..]
        .iter()
        .position(|l| is_separator(l))
        .map_or(lines.len(), |i| first + 1 + i);
    // The header `date - author <mail>`, then the body.
    let header = (first + 1..end).find(|&i| !lines[i].trim().is_empty())?;
    let entry = lines[header + 1..end].join("\n");
    let missing = missing_refs(&entry, refs);
    if missing.is_empty() {
        return None;
    }
    let suffix = refs_suffix(&missing);
    let mut out: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
    let bullet = (header + 1..end).find(|&i| lines[i].trim_start().starts_with("- "));
    match bullet {
        Some(b) => {
            // The bullet's text runs on over indented lines that do not start a
            // sub-list of their own.
            let mut last = b;
            for (i, line) in lines.iter().enumerate().take(end).skip(b + 1) {
                let t = line.trim_start();
                let continues =
                    line.starts_with(' ') && !t.is_empty() && !t.starts_with(['*', '-', '+']);
                if !continues {
                    break;
                }
                last = i;
            }
            let line = lines[last].trim_end();
            out[last] = match line.strip_suffix(':') {
                Some(head) => format!("{} {suffix}:", head.trim_end()),
                None => format!("{line} {suffix}"),
            };
        }
        None => out.insert(header + 1, format!("- References: {}", missing.join(", "))),
    }
    let mut text = out.join("\n");
    if changes.ends_with('\n') {
        text.push('\n');
    }
    Some(text)
}

/// The body of the newest `.changes` entry, without its header.
pub fn newest_entry_body(changes: &str) -> Option<String> {
    crate::update_project::latest_changes_entry(changes)
}
