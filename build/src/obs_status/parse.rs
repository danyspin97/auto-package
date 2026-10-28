use super::*;

/// One `<status>` of one `<result>` (repository/arch) element.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResultEntry {
    pub repo: String,
    pub arch: String,
    /// The `<result dirty="true">` flag: the scheduler has not yet
    /// recomputed this repository, so package codes may describe an older
    /// source revision. `osc` renders it as a `*` suffix.
    pub dirty: bool,
    pub pkg: String,
    pub code: String,
    /// `<details>` text — for `finished` this carries the real verdict.
    pub details: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Tag {
    name: String,
    attrs: Vec<(String, String)>,
    closing: bool,
    self_closing: bool,
    /// Byte offset just past the tag's `>`.
    end: usize,
}

impl Tag {
    fn attr(&self, key: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }
}

/// Next tag at or after `from`, skipping text, comments, `<?...?>` and
/// `<!...>`. `Ok(None)` at a clean end of input; `Err(())` when the input
/// ends inside a tag or comment (a cut-off document).
pub(super) fn scan_tag(xml: &str, mut from: usize) -> Result<Option<Tag>, ()> {
    loop {
        let Some(off) = xml.get(from..).and_then(|r| r.find('<')) else {
            return Ok(None);
        };
        let open = from + off;
        let rest = &xml[open..];
        if rest.starts_with("<!--") {
            from = open + rest.find("-->").ok_or(())? + 3;
            continue;
        }
        let close = open + rest.find('>').ok_or(())?;
        let inner = &xml[open + 1..close];
        if inner.starts_with('?') || inner.starts_with('!') {
            from = close + 1;
            continue;
        }
        let closing = inner.starts_with('/');
        let self_closing = inner.ends_with('/');
        let inner = inner.trim_start_matches('/').trim_end_matches('/');
        let (name, attrs) = match inner.find(char::is_whitespace) {
            Some(i) => (&inner[..i], crate::obs_xml::attrs(&inner[i..])),
            None => (inner, Vec::new()),
        };
        return Ok(Some(Tag {
            name: name.to_string(),
            attrs,
            closing,
            self_closing,
            end: close + 1,
        }));
    }
}

/// [`scan_tag`] for the lenient parsers: a truncated tag reads as the end.
pub(super) fn next_tag(xml: &str, from: usize) -> Option<Tag> {
    scan_tag(xml, from).ok().flatten()
}

/// Text between `from` and the next `<`, entity-decoded and trimmed.
pub(super) fn text_after(xml: &str, from: usize) -> String {
    let rest = xml.get(from..).unwrap_or("");
    crate::obs_xml::unescape(rest.split('<').next().unwrap_or("").trim())
}

/// Every package status in a `_result` document, across repositories/arches.
/// A cut-off document yields only the prefix; use [`try_parse_results`] when
/// the caller must not mistake that for the whole list.
pub fn parse_results(xml: &str) -> Vec<ResultEntry> {
    parse_results_inner(xml).0
}

/// [`parse_results`], but a truncated document is an error: otherwise every
/// package after the cut silently disappears and reads as "not built".
pub fn try_parse_results(xml: &str) -> Result<Vec<ResultEntry>, String> {
    match parse_results_inner(xml) {
        (entries, false) => Ok(entries),
        (entries, true) => Err(format!(
            "truncated _result document (kept {} entries before the cut)",
            entries.len()
        )),
    }
}

/// The entries read, and whether the document ended mid-tag or inside an
/// open `<result>`.
pub(super) fn parse_results_inner(xml: &str) -> (Vec<ResultEntry>, bool) {
    let mut entries: Vec<ResultEntry> = Vec::new();
    let (mut repo, mut arch, mut dirty) = (String::new(), String::new(), false);
    let mut in_result = false;
    let mut open_status: Option<usize> = None;
    let mut pos = 0;
    let truncated;
    loop {
        let tag = match scan_tag(xml, pos) {
            Ok(Some(tag)) => tag,
            Ok(None) => {
                truncated = in_result;
                break;
            }
            Err(()) => {
                truncated = true;
                break;
            }
        };
        pos = tag.end;
        match (tag.name.as_str(), tag.closing) {
            ("result", false) => {
                repo = tag.attr("repository").unwrap_or("").to_string();
                arch = tag.attr("arch").unwrap_or("").to_string();
                dirty = tag.attr("dirty") == Some("true");
                in_result = !tag.self_closing;
            }
            ("result", true) => {
                in_result = false;
                open_status = None;
            }
            ("status", false) if in_result => {
                let (Some(pkg), Some(code)) = (tag.attr("package"), tag.attr("code")) else {
                    continue;
                };
                entries.push(ResultEntry {
                    repo: repo.clone(),
                    arch: arch.clone(),
                    dirty,
                    pkg: pkg.to_string(),
                    code: code.to_string(),
                    details: None,
                });
                open_status = (!tag.self_closing).then(|| entries.len() - 1);
            }
            ("status", true) => open_status = None,
            ("details", false) if !tag.self_closing => {
                if let Some(i) = open_status {
                    let text = text_after(xml, tag.end);
                    if !text.is_empty() {
                        entries[i].details = Some(text);
                    }
                }
            }
            _ => {}
        }
    }
    (entries, truncated)
}

/// File names of the `.rpm` binaries in a `/build/<prj>/<repo>/<arch>/<pkg>`
/// listing (`<binarylist><binary filename="..." .../>`), source RPMs excluded.
pub fn parse_binary_rpms(xml: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut pos = 0;
    while let Some(tag) = next_tag(xml, pos) {
        pos = tag.end;
        if tag.name == "binary"
            && !tag.closing
            && let Some(f) = tag.attr("filename")
            && f.ends_with(".rpm")
            && !f.ends_with(".src.rpm")
        {
            out.push(f.to_string());
        }
    }
    out
}

/// The `.rpm` binaries of a package listing with their sizes in bytes (source
/// RPMs excluded; a binary with no readable size is left out).
pub fn parse_binary_rpm_sizes(xml: &str) -> Vec<(String, u64)> {
    let mut out = Vec::new();
    let mut pos = 0;
    while let Some(tag) = next_tag(xml, pos) {
        pos = tag.end;
        if tag.name == "binary"
            && !tag.closing
            && let Some(f) = tag.attr("filename")
            && f.ends_with(".rpm")
            && !f.ends_with(".src.rpm")
            && let Some(size) = tag.attr("size").and_then(|s| s.parse().ok())
        {
            out.push((f.to_string(), size));
        }
    }
    out
}

/// The entry for one package in one repository/arch, if present.
pub fn find_entry(xml: &str, repo: &str, arch: &str, pkg: &str) -> Option<ResultEntry> {
    entry_in(&parse_results(xml), repo, arch, pkg).cloned()
}

/// [`find_entry`] over entries that are already parsed, for a document shared
/// by several packages.
pub fn entry_in<'a>(
    entries: &'a [ResultEntry],
    repo: &str,
    arch: &str,
    pkg: &str,
) -> Option<&'a ResultEntry> {
    entries
        .iter()
        .find(|e| e.repo == repo && e.arch == arch && e.pkg == pkg)
}

/// The multibuild flavor entries of `pkg` (`pkg:flavor`) in one
/// repository/arch.
pub fn flavor_entries(xml: &str, repo: &str, arch: &str, pkg: &str) -> Vec<ResultEntry> {
    flavors_in(&parse_results(xml), repo, arch, pkg)
}

/// [`flavor_entries`] over entries that are already parsed.
pub fn flavors_in(entries: &[ResultEntry], repo: &str, arch: &str, pkg: &str) -> Vec<ResultEntry> {
    let prefix = format!("{pkg}:");
    entries
        .iter()
        .filter(|e| e.repo == repo && e.arch == arch && e.pkg.starts_with(&prefix))
        .cloned()
        .collect()
}

/// Whether a status says the package's newest sources have not been built
/// yet: a verdict of an older revision (`stale: ..`) or one no job history
/// could vouch for (`unconfirmed: ..`). Such a status is never an answer; the
/// wait goes on, and checks sooner and more often what the scheduler says.
pub fn awaiting_new_revision(s: &BuildStatus) -> bool {
    matches!(s, BuildStatus::Unknown(t) if t.starts_with("stale:") || t.starts_with("unconfirmed:"))
}

/// Source revision identity from `/source/<prj>/<pkg>?view=info`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceInfo {
    pub srcmd5: String,
    pub verifymd5: Option<String>,
}

pub fn parse_source_info(xml: &str) -> Option<SourceInfo> {
    let mut pos = 0;
    while let Some(tag) = next_tag(xml, pos) {
        pos = tag.end;
        if tag.name == "sourceinfo" && !tag.closing {
            return Some(SourceInfo {
                srcmd5: tag.attr("srcmd5").filter(|s| !s.is_empty())?.to_string(),
                verifymd5: tag
                    .attr("verifymd5")
                    .filter(|s| !s.is_empty())
                    .map(str::to_string),
            });
        }
    }
    None
}

/// One finished build job from `_jobhistory`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobInfo {
    pub srcmd5: String,
    pub verifymd5: Option<String>,
    pub code: String,
}

/// The newest job in a `_jobhistory` document (entries are oldest-first).
pub fn parse_latest_job(xml: &str) -> Option<JobInfo> {
    let mut latest = None;
    let mut pos = 0;
    while let Some(tag) = next_tag(xml, pos) {
        pos = tag.end;
        if tag.name == "jobhist" && !tag.closing {
            latest = Some(JobInfo {
                srcmd5: tag.attr("srcmd5").unwrap_or("").to_string(),
                verifymd5: tag
                    .attr("verifymd5")
                    .filter(|s| !s.is_empty())
                    .map(str::to_string),
                code: tag.attr("code").unwrap_or("").to_string(),
            });
        }
    }
    latest.filter(|j| !j.srcmd5.is_empty())
}

impl JobInfo {
    /// Was this job built from the revision currently on the server?
    pub fn is_for(&self, src: &SourceInfo) -> bool {
        self.srcmd5 == src.srcmd5
            || Some(&self.srcmd5) == src.verifymd5.as_ref()
            || (self.verifymd5.is_some() && self.verifymd5 == src.verifymd5)
    }
}
