//! Reading OBS build status from its XML API, and deciding what it means.
//!
//! `osc results` prints a human table: the verdict of a finished package is
//! hidden (`finished`), staleness is a trailing `*`, and the project-wide
//! layout is positional. Scraping that was the source of several hangs and
//! misreads, so status is read from `/build/<project>/_result` instead and
//! interpreted by [`resolve`], a pure function of the XML plus (only when
//! needed) the source revision and job history.
//!
//! No XML crate is pulled in for this: the documents are small, flat and
//! machine-generated, so a tag scanner with attribute decoding is enough —
//! and it is deliberately lenient (unknown tags/attributes are ignored,
//! attribute order and quote style don't matter, self-closing tags work).

use crate::osc::BuildStatus;

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
struct Tag {
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

fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    s.replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

/// Parse `key="value"` / `key='value'` pairs from the inside of a tag.
fn parse_attrs(mut s: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    loop {
        s = s.trim_start();
        let Some(eq) = s.find('=') else { break };
        let key = s[..eq].trim().to_string();
        let rest = s[eq + 1..].trim_start();
        let Some(quote) = rest.chars().next().filter(|c| *c == '"' || *c == '\'') else {
            break;
        };
        let body = &rest[1..];
        let Some(close) = body.find(quote) else { break };
        if !key.is_empty() {
            out.push((key, decode_entities(&body[..close])));
        }
        s = &body[close + 1..];
    }
    out
}

/// Next tag at or after `from`, skipping text, comments, `<?...?>` and
/// `<!...>`. `Ok(None)` at a clean end of input; `Err(())` when the input
/// ends inside a tag or comment (a cut-off document).
fn scan_tag(xml: &str, mut from: usize) -> Result<Option<Tag>, ()> {
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
            Some(i) => (&inner[..i], parse_attrs(&inner[i..])),
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
fn next_tag(xml: &str, from: usize) -> Option<Tag> {
    scan_tag(xml, from).ok().flatten()
}

/// Text between `from` and the next `<`, entity-decoded and trimmed.
fn text_after(xml: &str, from: usize) -> String {
    let rest = xml.get(from..).unwrap_or("");
    decode_entities(rest.split('<').next().unwrap_or("").trim())
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
fn parse_results_inner(xml: &str) -> (Vec<ResultEntry>, bool) {
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
    parse_results(xml)
        .into_iter()
        .find(|e| e.repo == repo && e.arch == arch && e.pkg == pkg)
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

/// Everything beyond the `_result` document itself that [`resolve`] may need.
#[derive(Debug, Default, Clone)]
pub struct Evidence {
    pub source: Option<SourceInfo>,
    pub job: Option<JobInfo>,
    /// The caller has just committed sources and waits for their build: a
    /// built verdict (`succeeded`/`failed`) is trusted only when the job
    /// history confirms it, dirty flag or not. Right after a commit the
    /// repository is not yet flagged dirty, so the previous revision's
    /// verdict would otherwise read as the answer.
    pub confirm: bool,
}

/// Whether [`resolve`] needs [`Evidence`] for this entry — lets callers skip
/// two API calls in the common case. `confirm` is [`Evidence::confirm`].
pub fn needs_evidence(entry: &ResultEntry, confirm: bool) -> bool {
    verdict(entry).is_some_and(|v| {
        (entry.dirty && revision_dependent(&v)) || (confirm && is_built(&v, verdict_word(entry)))
    })
}

/// A verdict that comes from running a build (the job history records it),
/// as opposed to a scheduler judgement like `unresolvable`, `excluded` or
/// `unchanged` (no build was run: the sources produce what is already there).
fn is_built(s: &BuildStatus, word: &str) -> bool {
    matches!(s, BuildStatus::Succeeded | BuildStatus::Failed) && word != "unchanged"
}

/// A package's verdict word: the code itself, or `<details>` for `finished`.
fn verdict_word(entry: &ResultEntry) -> &str {
    if entry.code == "finished" {
        entry.details.as_deref().unwrap_or("finished")
    } else {
        &entry.code
    }
}

fn verdict(entry: &ResultEntry) -> Option<BuildStatus> {
    let s = BuildStatus::from(verdict_word(entry));
    s.is_terminal().then_some(s)
}

/// Disabled/excluded come from project configuration, not from a source
/// revision, so a dirty repository cannot make them stale.
fn revision_dependent(s: &BuildStatus) -> bool {
    !matches!(s, BuildStatus::Disabled | BuildStatus::Excluded)
}

fn word_status(word: &str) -> BuildStatus {
    match word {
        // Transitional: the binary is still being published, so a dependent must
        // wait for it rather than schedule a second build of the same package.
        "dispatching" | "signing" | "publishing" | "unpublished" => BuildStatus::Building,
        // `finished` with the details `unchanged`: OBS found the new sources
        // build to the same result and kept the previous binaries.
        "unchanged" => BuildStatus::Succeeded,
        other => BuildStatus::from(other),
    }
}

/// Decide what one package's status means.
///
/// * Active states (`scheduled`, `building`, ...) map straight across.
/// * A bare `finished` is resolved through its `<details>` verdict.
/// * A terminal verdict in a `dirty` repository (or, with
///   [`Evidence::confirm`], any built verdict) may describe an older source
///   revision, so it is only trusted when the newest job in the job history
///   was built from the revision currently on the server — and then the
///   job's own result code wins. Otherwise the result is
///   `Unknown("stale: <word>")`, which callers must keep waiting on.
pub fn resolve(entry: &ResultEntry, evidence: &Evidence) -> BuildStatus {
    let word = verdict_word(entry);
    let status = word_status(word);
    if !status.is_terminal() {
        return match (&status, entry.code.as_str()) {
            // `finished` with no readable verdict: not actionable yet.
            (BuildStatus::Unknown(_), "finished") => BuildStatus::Unknown("finished".to_string()),
            _ => status,
        };
    }
    let must_confirm = (entry.dirty && revision_dependent(&status))
        || (evidence.confirm && is_built(&status, word));
    if !must_confirm {
        return status;
    }
    if let (Some(src), Some(job)) = (&evidence.source, &evidence.job)
        && job.is_for(src)
    {
        let from_job = BuildStatus::from(job.code.as_str());
        if from_job.is_terminal() {
            return from_job;
        }
    }
    BuildStatus::Unknown(format!("stale: {word}"))
}

/// Resolve ignoring staleness: the dirty flag is dropped, so every terminal
/// verdict is taken at face value. For project-wide scans that only decide
/// what is worth retrying, where evidence per package would cost two API
/// calls each.
pub fn resolve_lenient(entry: &ResultEntry) -> BuildStatus {
    let mut e = entry.clone();
    e.dirty = false;
    resolve(&e, &Evidence::default())
}

#[cfg(test)]
mod tests {
    use super::*;

    const REAL: &str = r#"<resultlist state="198a2ea6aaa8c09f5cc9e4de90d1fa74">
  <result project="home:u:p" repository="openSUSE_Tumbleweed" arch="i586" code="published" state="published">
    <status package="oatpp" code="failed"/>
  </result>
  <result project="home:u:p" repository="openSUSE_Tumbleweed" arch="x86_64" code="building" state="building" dirty="true">
    <status package="oatpp" code="finished">
      <details>failed</details>
    </status>
    <status package="other" code="blocked"><details>waiting for 1 compiles</details></status>
  </result>
</resultlist>"#;

    fn entry(code: &str, details: Option<&str>, dirty: bool) -> ResultEntry {
        ResultEntry {
            repo: "r".into(),
            arch: "a".into(),
            dirty,
            pkg: "p".into(),
            code: code.into(),
            details: details.map(str::to_string),
        }
    }

    fn evidence(src: &str, job_src: &str, job_code: &str) -> Evidence {
        Evidence {
            source: Some(SourceInfo {
                srcmd5: src.into(),
                verifymd5: None,
            }),
            job: Some(JobInfo {
                srcmd5: job_src.into(),
                verifymd5: None,
                code: job_code.into(),
            }),
            confirm: false,
        }
    }

    #[test]
    fn parses_real_result_document() {
        let all = parse_results(REAL);
        assert_eq!(all.len(), 3);
        let x86 = find_entry(REAL, "openSUSE_Tumbleweed", "x86_64", "oatpp").unwrap();
        assert!(x86.dirty);
        assert_eq!(x86.code, "finished");
        assert_eq!(x86.details.as_deref(), Some("failed"));
        let i586 = find_entry(REAL, "openSUSE_Tumbleweed", "i586", "oatpp").unwrap();
        assert!(!i586.dirty);
        assert_eq!(i586.details, None);
        let other = find_entry(REAL, "openSUSE_Tumbleweed", "x86_64", "other").unwrap();
        assert_eq!(other.details.as_deref(), Some("waiting for 1 compiles"));
        assert!(find_entry(REAL, "openSUSE_Tumbleweed", "x86_64", "nope").is_none());
    }

    #[test]
    fn a_cut_off_document_is_an_error() {
        let cut = &REAL[..REAL.find("package=\"other\"").unwrap() + 5];
        assert!(try_parse_results(cut).is_err());
        // Cut between tags, inside an open <result>.
        let cut = &REAL[..REAL.find("<status package=\"other\"").unwrap()];
        assert!(try_parse_results(cut).is_err());
        assert_eq!(try_parse_results(REAL).unwrap().len(), 3);
        assert!(try_parse_results("").unwrap().is_empty());
    }

    #[test]
    fn attribute_order_quotes_and_entities_do_not_matter() {
        let xml = "<?xml version='1.0'?><resultlist><result arch='x86_64' dirty='true' repository='r'>\
                   <status code='succeeded' package='a&amp;b'/></result></resultlist>";
        let e = &parse_results(xml)[0];
        assert_eq!(
            (e.pkg.as_str(), e.code.as_str(), e.dirty),
            ("a&b", "succeeded", true)
        );
    }

    #[test]
    fn garbage_and_truncation_yield_nothing_not_a_panic() {
        assert!(parse_results("").is_empty());
        assert!(parse_results("not xml at all").is_empty());
        assert!(
            parse_results("<resultlist><result repository=\"r\" arch=\"a\"><status pack")
                .is_empty()
        );
        assert!(parse_source_info("<sourceinfo srcmd5=").is_none());
        assert!(parse_latest_job("<jobhistlist><jobhist srcmd5=\"").is_none());
    }

    #[test]
    fn active_states_map_directly() {
        for (code, want) in [
            ("scheduled", BuildStatus::Scheduled),
            ("building", BuildStatus::Building),
            ("dispatching", BuildStatus::Building),
            ("signing", BuildStatus::Building),
            ("blocked", BuildStatus::Blocked),
        ] {
            assert_eq!(
                resolve(&entry(code, None, true), &Evidence::default()),
                want
            );
        }
        assert_eq!(
            resolve(&entry("locked", None, false), &Evidence::default()),
            BuildStatus::Unknown("locked".into())
        );
    }

    #[test]
    fn clean_repo_trusts_the_verdict_including_finished_details() {
        let none = Evidence::default();
        assert_eq!(
            resolve(&entry("succeeded", None, false), &none),
            BuildStatus::Succeeded
        );
        assert_eq!(
            resolve(&entry("finished", Some("succeeded"), false), &none),
            BuildStatus::Succeeded
        );
        assert_eq!(
            resolve(&entry("finished", Some("failed"), false), &none),
            BuildStatus::Failed
        );
    }

    #[test]
    fn bare_finished_without_details_is_not_terminal() {
        let s = resolve(&entry("finished", None, false), &Evidence::default());
        assert_eq!(s, BuildStatus::Unknown("finished".into()));
        assert!(!s.is_terminal());
    }

    #[test]
    fn dirty_repo_without_evidence_is_stale() {
        let s = resolve(&entry("succeeded", None, true), &Evidence::default());
        assert_eq!(s, BuildStatus::Unknown("stale: succeeded".into()));
    }

    #[test]
    fn dirty_repo_with_matching_job_uses_the_jobs_verdict() {
        let e = entry("finished", Some("failed"), true);
        assert_eq!(
            resolve(&e, &evidence("abc", "abc", "succeeded")),
            BuildStatus::Succeeded
        );
        assert_eq!(
            resolve(&e, &evidence("abc", "abc", "failed")),
            BuildStatus::Failed
        );
    }

    #[test]
    fn dirty_repo_with_job_for_an_older_revision_stays_stale() {
        let e = entry("finished", Some("failed"), true);
        let s = resolve(&e, &evidence("new", "old", "failed"));
        assert_eq!(s, BuildStatus::Unknown("stale: failed".into()));
    }

    #[test]
    fn verifymd5_counts_as_the_same_revision() {
        let src = SourceInfo {
            srcmd5: "expanded".into(),
            verifymd5: Some("verify".into()),
        };
        let job = |srcmd5: &str, v: Option<&str>| JobInfo {
            srcmd5: srcmd5.into(),
            verifymd5: v.map(str::to_string),
            code: "succeeded".into(),
        };
        assert!(job("verify", None).is_for(&src));
        assert!(job("other", Some("verify")).is_for(&src));
        assert!(!job("other", Some("different")).is_for(&src));
        assert!(!job("other", None).is_for(&src));
    }

    #[test]
    fn disabled_and_excluded_are_never_stale() {
        let none = Evidence::default();
        assert_eq!(
            resolve(&entry("disabled", None, true), &none),
            BuildStatus::Disabled
        );
        assert_eq!(
            resolve(&entry("excluded", None, true), &none),
            BuildStatus::Excluded
        );
        assert!(!needs_evidence(&entry("excluded", None, true), false));
    }

    /// python-nvidia-cutlass-dsl-libs-base as OBS reported it: excluded on
    /// i586, succeeded on x86_64, in one document. Each pair resolves to its
    /// own state whichever order the document lists them in, and the x86_64
    /// `building` of the repository itself does not leak into the package.
    #[test]
    fn each_repository_arch_resolves_to_its_own_status() {
        let xml = r#"<resultlist>
  <result project="p" repository="openSUSE_Tumbleweed" arch="i586" code="published" state="published">
    <status package="python-nvidia-cutlass-dsl-libs-base" code="excluded"/>
  </result>
  <result project="p" repository="openSUSE_Tumbleweed" arch="x86_64" code="building" state="building">
    <status package="python-nvidia-cutlass-dsl-libs-base" code="succeeded"/>
  </result>
</resultlist>"#;
        let status = |arch: &str| {
            let e = find_entry(
                xml,
                "openSUSE_Tumbleweed",
                arch,
                "python-nvidia-cutlass-dsl-libs-base",
            )
            .unwrap();
            resolve(&e, &Evidence::default())
        };
        assert_eq!(status("x86_64"), BuildStatus::Succeeded);
        assert_eq!(status("i586"), BuildStatus::Excluded);
        assert!(
            find_entry(
                xml,
                "openSUSE_Tumbleweed",
                "aarch64",
                "python-nvidia-cutlass-dsl-libs-base"
            )
            .is_none()
        );
    }

    #[test]
    fn binary_sizes_skip_source_rpms_and_non_rpms() {
        let listing = r#"<binarylist>
  <binary filename="_buildenv" size="348736" mtime="1791824718"/>
  <binary filename="python-cuda-bindings-13.4.3-2.1.src.rpm" size="3285054" mtime="1791824718"/>
  <binary filename="python313-cuda-bindings-13.4.3-2.1.x86_64.rpm" size="11053" mtime="1791824718"/>
  <binary filename="python314-cuda-bindings-13.4.3-2.1.x86_64.rpm" size="10925" mtime="1791824718"/>
  <binary filename="rpmlint.log" size="2502" mtime="1791824717"/>
</binarylist>"#;
        assert_eq!(
            parse_binary_rpm_sizes(listing),
            vec![
                (
                    "python313-cuda-bindings-13.4.3-2.1.x86_64.rpm".to_string(),
                    11053
                ),
                (
                    "python314-cuda-bindings-13.4.3-2.1.x86_64.rpm".to_string(),
                    10925
                ),
            ]
        );
    }

    #[test]
    fn unresolvable_details_stay_with_their_own_result() {
        let xml = r#"<resultlist>
  <result repository="r" arch="i586">
    <status package="p" code="unresolvable"><details>nothing provides python314-fastar, nothing provides python313-fastar</details></status>
  </result>
  <result repository="r" arch="x86_64">
    <status package="p" code="succeeded"/>
  </result>
</resultlist>"#;
        let i586 = find_entry(xml, "r", "i586", "p").unwrap();
        assert_eq!(
            i586.details.as_deref(),
            Some("nothing provides python314-fastar, nothing provides python313-fastar")
        );
        assert_eq!(find_entry(xml, "r", "x86_64", "p").unwrap().details, None);
    }

    #[test]
    fn finished_unchanged_is_a_success_that_needs_no_new_job() {
        let e = entry("finished", Some("unchanged"), false);
        let with_confirm = Evidence {
            confirm: true,
            ..Default::default()
        };
        assert_eq!(resolve(&e, &with_confirm), BuildStatus::Succeeded);
        assert!(!needs_evidence(&e, true));
    }

    #[test]
    fn needs_evidence_only_for_dirty_terminal_results() {
        assert!(needs_evidence(&entry("succeeded", None, true), false));
        assert!(needs_evidence(
            &entry("finished", Some("failed"), true),
            false
        ));
        assert!(!needs_evidence(&entry("succeeded", None, false), false));
        assert!(!needs_evidence(&entry("building", None, true), false));
    }

    #[test]
    fn after_a_commit_a_clean_built_verdict_needs_the_job_history() {
        assert!(needs_evidence(&entry("failed", None, false), true));
        assert!(needs_evidence(&entry("succeeded", None, false), true));
        // Scheduler judgements are not in the job history.
        assert!(!needs_evidence(&entry("unresolvable", None, false), true));
        assert!(!needs_evidence(&entry("excluded", None, false), true));
        assert!(!needs_evidence(&entry("building", None, false), true));
    }

    #[test]
    fn after_a_commit_the_previous_revisions_verdict_is_stale_even_when_clean() {
        let e = entry("failed", None, false);
        let confirm = |mut ev: Evidence| {
            ev.confirm = true;
            ev
        };
        // The newest job is for the old sources.
        assert_eq!(
            resolve(&e, &confirm(evidence("new", "old", "failed"))),
            BuildStatus::Unknown("stale: failed".into())
        );
        // No evidence at all is not a confirmation.
        assert_eq!(
            resolve(&e, &confirm(Evidence::default())),
            BuildStatus::Unknown("stale: failed".into())
        );
        // The job for the new sources decides.
        assert_eq!(
            resolve(&e, &confirm(evidence("new", "new", "succeeded"))),
            BuildStatus::Succeeded
        );
        // Not asked to confirm: the clean verdict stands.
        assert_eq!(
            resolve(&e, &evidence("new", "old", "failed")),
            BuildStatus::Failed
        );
    }

    #[test]
    fn lenient_resolution_ignores_dirty() {
        assert_eq!(
            resolve_lenient(&entry("failed", None, true)),
            BuildStatus::Failed
        );
    }

    #[test]
    fn latest_job_is_the_last_entry_and_source_info_parses() {
        let hist = r#"<jobhistlist>
  <jobhist package="p" rev="11" srcmd5="aaa" code="failed" verifymd5="aaa"/>
  <jobhist package="p" rev="12" srcmd5="bbb" code="succeeded"/>
</jobhistlist>"#;
        let j = parse_latest_job(hist).unwrap();
        assert_eq!((j.srcmd5.as_str(), j.code.as_str()), ("bbb", "succeeded"));
        assert!(parse_latest_job("<jobhistlist></jobhistlist>").is_none());
        let src = parse_source_info(
            r#"<sourceinfo package="p" rev="12" srcmd5="bbb" verifymd5="ccc"><filename>p.spec</filename></sourceinfo>"#,
        )
        .unwrap();
        assert_eq!(
            (src.srcmd5.as_str(), src.verifymd5.as_deref()),
            ("bbb", Some("ccc"))
        );
    }

    #[test]
    fn binary_list_yields_binary_rpms_only() {
        let xml = r#"<binarylist>
  <binary filename="_buildenv" size="1" mtime="1"/>
  <binary filename="python313-Events-0.5-2.1.noarch.rpm" size="1" mtime="1"/>
  <binary filename="python-events-0.5-2.1.src.rpm" size="1" mtime="1"/>
  <binary filename="buildinfo" size="1" mtime="1"/>
</binarylist>"#;
        assert_eq!(
            parse_binary_rpms(xml),
            ["python313-Events-0.5-2.1.noarch.rpm"]
        );
    }
}
