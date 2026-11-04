//! The fixes `fix --request` makes without a model, one pure function each on
//! the text of a file. A function that cannot be sure of the result refuses
//! with the reason, and the finding goes to [`MechanicalFix::fallback`].
//!
//! The `.changes` fixes touch only the entries the request adds: what stands
//! above the target's text (`own_prefix`). A request whose file does not end
//! with the target's text is the case of `changes.old-entries-edited`, which
//! [`restore_old_entries`] repairs and every other fix refuses.

use regex::Regex;

use crate::osc::is_changes_separator;
use crate::request_review::changes_checks::{is_dependency_bullet, update_target};

const SEPARATOR: &str = "-------------------------------------------------------------------";

/// The entries the request adds: `new` without the target's text at its end.
/// A package the target does not have is all new.
fn own_prefix<'a>(new: &'a str, base: Option<&str>) -> Result<&'a str, String> {
    let prefix = match base {
        None => new,
        Some(base) => new
            .strip_suffix(base)
            .ok_or("the old entries are not kept as the target has them")?,
    };
    if prefix.lines().any(is_changes_separator) {
        Ok(prefix)
    } else {
        Err("the request adds no entry".to_string())
    }
}

/// `new` with its own entries replaced by `own`.
fn with_own(new: &str, prefix: &str, own: String) -> String {
    format!("{own}{}", &new[prefix.len()..])
}

/// The entries of the request that hold two or more `- Refresh <patch>`
/// bullets get one grouped bullet, in the place of the first.
pub fn group_refresh_bullets(new: &str, base: Option<&str>) -> Result<String, String> {
    let prefix = own_prefix(new, base)?;
    let refresh = |l: &str| -> Option<String> {
        let name = l.strip_prefix("- Refresh ")?;
        (!name.starts_with("patches") && !name.trim().is_empty() && !name.contains(' '))
            .then(|| name.trim().to_string())
    };
    let mut out: Vec<String> = Vec::new();
    let mut changed = false;
    let mut entry: Vec<&str> = Vec::new();
    let flush = |entry: &mut Vec<&str>, out: &mut Vec<String>, changed: &mut bool| {
        let names: Vec<String> = entry.iter().filter_map(|l| refresh(l)).collect();
        if names.len() < 2 {
            out.extend(entry.drain(..).map(str::to_string));
            return;
        }
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let grouped = format!("- {}", crate::patch_check::refresh_bullet(&refs));
        let mut placed = false;
        for l in entry.drain(..) {
            if refresh(l).is_some() {
                if !placed {
                    out.push(grouped.clone());
                    placed = true;
                }
            } else {
                out.push(l.to_string());
            }
        }
        *changed = true;
    };
    for l in prefix.lines() {
        if is_changes_separator(l) {
            flush(&mut entry, &mut out, &mut changed);
        }
        entry.push(l);
    }
    flush(&mut entry, &mut out, &mut changed);
    if !changed {
        return Err("no entry has two Refresh bullets".into());
    }
    let own = format!("{}\n", out.join("\n"));
    Ok(with_own(new, prefix, own))
}

/// The target's text with the request's new entries above it: what the request
/// added is what stands above the target's newest entry. Refuses when the
/// target's newest entry is not in the file, or nothing stands above it.
pub fn restore_old_entries(new: &str, base: Option<&str>) -> Result<String, String> {
    let base = base.ok_or("the target has no .changes to restore")?;
    if new.ends_with(base) {
        return Err("the old entries are kept already".into());
    }
    let newest = base
        .split(&format!("\n{SEPARATOR}\n"))
        .next()
        .unwrap_or(base)
        .trim();
    // The target's entries are the last copy of its newest entry: the new entries
    // above may quote the same text.
    let at = (!newest.is_empty())
        .then(|| new.rfind(newest))
        .flatten()
        .ok_or(
            "the target's newest entry is not in the file: the new entries cannot be told apart",
        )?;
    let own = &new[..at];
    if !own
        .lines()
        .any(|l| !l.trim().is_empty() && !is_changes_separator(l))
    {
        return Err("nothing stands above the target's entries".into());
    }
    Ok(format!("{own}{base}"))
}

/// `- Update to <version>` first in the newest entry of the request, when no
/// entry of the request has an `Update to` bullet at all (one that names
/// another version is for a person or a turn to judge).
pub fn add_update_bullet(new: &str, base: Option<&str>, version: &str) -> Result<String, String> {
    let prefix = own_prefix(new, base)?;
    if prefix
        .lines()
        .filter_map(|l| l.strip_prefix("- "))
        .any(|b| update_target(b.trim()).is_some())
    {
        return Err("an entry has an Update to bullet that names another version".into());
    }
    let mut lines: Vec<&str> = prefix.lines().collect();
    let sep = lines
        .iter()
        .position(|l| is_changes_separator(l))
        .ok_or("the request adds no entry")?;
    let header = (sep + 1..lines.len())
        .find(|&i| !lines[i].trim().is_empty())
        .ok_or("the entry has no header")?;
    let mut at = header + 1;
    if lines.get(at).is_some_and(|l| l.trim().is_empty()) {
        at += 1;
    }
    let bullet = format!("- Update to {version}");
    lines.insert(at, &bullet);
    let own = format!("{}\n", lines.join("\n"));
    Ok(with_own(new, prefix, own))
}

/// A bullet for a patch the request adds and no bullet names: `Add <file>:
/// <subject>` from the patch's `Subject:`. `patch` is the patch text; without
/// a subject the fix refuses (a turn says what the patch does).
pub fn add_patch_bullet(
    new: &str,
    base: Option<&str>,
    file: &str,
    patch: &str,
) -> Result<String, String> {
    own_prefix(new, base)?;
    let subject = crate::backport::subject_of_patch(patch)
        .map(|s| s.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("{file} has no Subject: to make a bullet from"))?;
    let subject: String = subject.chars().take(100).collect();
    let bullet = format!("- Add {file}: {subject}");
    crate::backport::with_backport_bullets(new, &[(file.to_string(), bullet)])
        .ok_or_else(|| "the bullet is there already".to_string())
}

/// The entry headers of the request in the standard form, and a separator of
/// 67 dashes: the same date, name and mail, written as `osc vc` writes them.
pub fn normalise_headers(new: &str, base: Option<&str>) -> Result<String, String> {
    let prefix = own_prefix(new, base)?;
    let loose = Regex::new(
        r"^(Mon|Tue|Wed|Thu|Fri|Sat|Sun)\w* (Jan|Feb|Mar|Apr|May|Jun|Jul|Aug|Sep|Oct|Nov|Dec)\w* +(\d{1,2}) (\d{2}:\d{2}:\d{2}) (?:UTC |GMT )?(\d{4}) *- *(.+?) *<([^<>\s@]+@[^<>\s]+)> *$",
    )
    .map_err(|e| e.to_string())?;
    let mut lines: Vec<String> = prefix.lines().map(str::to_string).collect();
    let mut changed = false;
    let mut i = 0;
    while i < lines.len() {
        if is_changes_separator(&lines[i]) {
            if lines[i] != SEPARATOR {
                lines[i] = SEPARATOR.to_string();
                changed = true;
            }
            if let Some(h) = lines.get(i + 1)
                && !is_changes_separator(h)
            {
                let c = loose.captures(h).ok_or_else(|| {
                    format!("the header {h:?} does not hold a date, a name and a mail")
                })?;
                let fixed = format!(
                    "{} {} {:>2} {} UTC {} - {} <{}>",
                    &c[1], &c[2], &c[3], &c[4], &c[5], &c[6], &c[7]
                );
                if fixed != *h {
                    lines[i + 1] = fixed;
                    changed = true;
                }
            }
        }
        i += 1;
    }
    if !changed {
        return Err("the headers are in the standard form".into());
    }
    let own = format!("{}\n", lines.join("\n"));
    Ok(with_own(new, prefix, own))
}

/// The bullets of the request's entries that only say a dependency changed are
/// dropped. Refuses when that would leave an entry without a bullet.
pub fn drop_dependency_bullets(new: &str, base: Option<&str>) -> Result<String, String> {
    let prefix = own_prefix(new, base)?;
    let lines: Vec<&str> = prefix.lines().collect();
    let mut out: Vec<&str> = Vec::new();
    let (mut dropped, mut kept_in_entry, mut dropped_in_entry) = (0, 0, 0);
    let mut i = 0;
    let end_entry = |kept: usize, dropped: usize| -> Result<(), String> {
        if dropped > 0 && kept == 0 {
            Err("dropping the dependency bullets would leave an entry without a bullet".into())
        } else {
            Ok(())
        }
    };
    while i < lines.len() {
        let l = lines[i];
        if is_changes_separator(l) {
            end_entry(kept_in_entry, dropped_in_entry)?;
            (kept_in_entry, dropped_in_entry) = (0, 0);
        }
        if let Some(text) = l.strip_prefix("- ") {
            let mut j = i + 1;
            let mut full = text.trim().to_string();
            while j < lines.len()
                && lines[j].starts_with(' ')
                && !lines[j].trim().is_empty()
                && !lines[j].trim_start().starts_with("- ")
            {
                full.push(' ');
                full.push_str(lines[j].trim());
                j += 1;
            }
            if update_target(&full).is_none() && is_dependency_bullet(&full) {
                dropped += 1;
                dropped_in_entry += 1;
                i = j;
                continue;
            }
            kept_in_entry += 1;
        }
        out.push(l);
        i += 1;
    }
    end_entry(kept_in_entry, dropped_in_entry)?;
    if dropped == 0 {
        return Err("no bullet only says a dependency changed".into());
    }
    let own = format!("{}\n", out.join("\n"));
    Ok(with_own(new, prefix, own))
}

/// Whether `name`, a file of the checkout, may be removed as a leftover: it
/// looks like one and the spec names it nowhere.
pub fn removable_leftover(name: &str, spec: &str, had_series: bool) -> Result<(), String> {
    if name.contains('/') || (name.starts_with('.') && !name.starts_with(".#")) {
        return Err(format!("{name} is not a plain file name"));
    }
    if spec.contains(name) {
        return Err(format!("the spec names {name}"));
    }
    if crate::request_review::source_checks::is_leftover_name(name, had_series) {
        return Ok(());
    }
    // An archive the spec no longer names: the check says it is still in the package.
    if !spec.contains(name)
        && [
            ".tar.gz", ".tar.bz2", ".tar.xz", ".tar.zst", ".tgz", ".zip", ".gem", ".crate",
            ".obscpio",
        ]
        .iter()
        .any(|s| name.ends_with(s))
    {
        return Ok(());
    }
    Err(format!("{name} does not look like a leftover"))
}

/// The bullets of the request's own newest entry replaced by `bullets` (one
/// per line, each starting `- ` or not), the header kept. Refuses when the
/// entry is not the request's, when `bullets` has none, or when it drops a
/// bullet that names one of `patches`, says `Update to <version>` or cites a
/// reference (`bsc#`, `CVE-`, a link): those are facts of the change.
pub fn apply_changes_entry(
    new: &str,
    base: Option<&str>,
    bullets: &str,
    patches: &[String],
) -> Result<String, String> {
    let prefix = own_prefix(new, base)?;
    let lines: Vec<&str> = prefix.lines().collect();
    let sep = lines
        .iter()
        .position(|l| is_changes_separator(l))
        .ok_or("the request adds no entry")?;
    let header = (sep + 1..lines.len())
        .find(|&i| !lines[i].trim().is_empty())
        .ok_or("the entry has no header")?;
    let end = (header + 1..lines.len())
        .find(|&i| is_changes_separator(lines[i]))
        .unwrap_or(lines.len());
    let old = lines[header + 1..end].join("\n");
    let fresh: Vec<String> = bullets
        .lines()
        .map(str::trim_end)
        .filter(|l| !l.trim().is_empty())
        .map(|l| {
            if l.starts_with(' ') || l.starts_with("- ") {
                l.to_string()
            } else {
                format!("- {l}")
            }
        })
        .collect();
    if !fresh.iter().any(|l| l.starts_with("- ")) {
        return Err("changes_entry holds no bullet".into());
    }
    let fresh_text = fresh.join("\n");
    let fact = |b: &str| {
        update_target(b).is_some()
            || [
                "bsc#", "boo#", "bnc#", "jsc#", "gh#", "CVE-", "http://", "https://",
            ]
            .iter()
            .any(|r| b.contains(r))
            || patches.iter().any(|p| b.contains(p.as_str()))
    };
    for b in old.lines().filter_map(|l| l.strip_prefix("- ")) {
        if fact(b) && !fresh_text.contains(b.trim()) {
            return Err(format!("changes_entry drops the bullet `{}`", b.trim()));
        }
    }
    let mut out: Vec<String> = lines[..=header].iter().map(|l| l.to_string()).collect();
    out.push(String::new());
    out.extend(fresh);
    out.push(String::new());
    out.extend(lines[end..].iter().map(|l| l.to_string()));
    let own = format!("{}\n", out.join("\n"));
    Ok(with_own(new, prefix, own))
}
