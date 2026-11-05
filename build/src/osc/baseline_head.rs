//! What a restore says when OBS is not at a revision the baseline may restore
//! over: the revision OBS holds, who committed it and when, what the baseline
//! kept, and whether an attempt of this package made that revision.

use super::*;

/// The newest revision of a package's history (`/source/<p>/<pkg>/_history`).
#[derive(Debug, PartialEq, Eq)]
pub struct LatestRevision {
    pub rev: String,
    /// The srcmd5 of the revision: for a link, its own revision (`lsrcmd5`).
    pub srcmd5: String,
    pub user: String,
    /// Unix time of the commit.
    pub time: i64,
    pub comment: String,
}

/// The newest `<revision>` of a `_history` listing.
pub fn latest_revision(history_xml: &str) -> Option<LatestRevision> {
    let start = history_xml.rfind("<revision ")?;
    let entry = &history_xml[start..];
    let rev = crate::obs_xml::attr(&entry[..entry.find('>')?], "rev")?;
    let text = |tag: &str| -> Option<String> {
        let open = format!("<{tag}>");
        let from = entry.find(&open)? + open.len();
        let to = from + entry[from..].find(&format!("</{tag}>"))?;
        Some(
            entry[from..to]
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" "),
        )
    };
    Some(LatestRevision {
        rev,
        srcmd5: text("srcmd5").unwrap_or_default(),
        user: text("user").unwrap_or_default(),
        time: text("time")?.parse().ok()?,
        comment: text("comment").unwrap_or_default(),
    })
}

/// `secs` as the UTC time `osc log` shows, `YYYY-MM-DD HH:MM:SS`.
pub fn utc_stamp(secs: i64) -> String {
    // Howard Hinnant's civil-from-days, on days since 1970-01-01.
    let z = secs.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    let of_day = secs.rem_euclid(86_400);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}",
        of_day / 3600,
        of_day / 60 % 60,
        of_day % 60
    )
}

/// The newest revision of `project/package` on OBS.
pub fn source_history(project: &str, package: &str) -> Result<LatestRevision, String> {
    let xml = source_history_xml(project, package, Emit::quiet()).map_err(|e| e.to_string())?;
    latest_revision(&xml).ok_or_else(|| format!("OBS history of {project}/{package} is empty"))
}

/// The refusal of a restore whose checkout may not be restored over: OBS holds
/// `head`, the checkout `local`. `own` is the package's own srcmd5 from the
/// unexpanded listing (for a link, its `lsrcmd5`). The refusal names the
/// revision, its committer and time, what the baseline kept, and whether an
/// attempt of this package committed it.
pub fn moved_on_reason(
    project: &str,
    package: &str,
    head: &str,
    local: &str,
    own: Option<&str>,
    dir: &Path,
    history: Result<LatestRevision, String>,
) -> String {
    if head != local && committed_revisions(dir).iter().any(|r| r == head) {
        return format!(
            "OBS moved on: {project}/{package} is at revision {head}, the checkout at {local}: \
             {head} is a revision an attempt of this package committed, but the checkout does \
             not hold it; nothing was written to OBS"
        );
    }
    if head == local {
        let in_flight = if pending_commit_base(dir).is_some() {
            "; an attempt commit may not have been recorded, so this revision may be that commit"
        } else {
            ""
        };
        return format!(
            "OBS holds revision {head}, which no attempt of this package committed, and the \
             checkout holds it too (it was updated from OBS after the attempt, or the attempt's \
             commit is not on record){in_flight}: the baseline is not restored over it; nothing was written to OBS"
        );
    }
    let mut why =
        format!("OBS moved on: {project}/{package} is at revision {head}, the checkout at {local}");
    match history {
        Ok(latest) if own.is_some_and(|o| o == latest.srcmd5) => why.push_str(&format!(
            " (the link's base moved: the package's own newest revision is {} by {} at {} UTC, \
             \"{}\", so the commit is not the package's)",
            latest.rev,
            latest.user,
            utc_stamp(latest.time),
            latest.comment
        )),
        Ok(latest) => why.push_str(&format!(
            " (newest OBS revision {} by {} at {} UTC: \"{}\")",
            latest.rev,
            latest.user,
            utc_stamp(latest.time),
            latest.comment
        )),
        Err(e) => why.push_str(&format!(" (its history is not readable: {e})")),
    }
    let attempted = {
        let mut names = attempted_files(dir);
        names.sort();
        names.dedup();
        names
    };
    why.push_str(&format!(
        "; the baseline kept {} attempted file(s){}",
        attempted.len(),
        if attempted.is_empty() {
            String::new()
        } else {
            format!(" ({})", attempted.join(", "))
        }
    ));
    if pending_commit_base(dir).is_some() {
        why.push_str(
            "; an attempt commit may not have been recorded, so this \
             revision may be that commit, which osc did not record",
        );
    }
    format!("{why}; nothing was written to OBS")
}
