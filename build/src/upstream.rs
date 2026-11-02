//! Finding the newest upstream version of a package that is neither on PyPI
//! nor in a git forge: in a directory listing of release tarballs (GNU,
//! kernel.org, a project's own download directory), in SourceForge's file
//! feed, or, when the spec says nothing usable about where the source comes
//! from, in Repology's record of what the distributions package.
//!
//! These only answer "what is the newest version"; building at that version is
//! a separate question, see [`crate::update::Ecosystem::can_build`].

use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::sources::{archive_name_version, fetch_index, index_dir, is_any_archive, last_segment};
use crate::version::{is_prerelease, sort_cmp as version_cmp};

const USER_AGENT: &str = "auto-package";

/// Repology asks for at most one request a second.
const REPOLOGY_SPACING: Duration = Duration::from_secs(1);

/// The newest release in the directory `source` sits in: the highest version
/// among the archives that carry the same project name as the file `source`
/// names. SourceForge file URLs are read from the project's feed instead.
pub fn latest_in_index(source: &str) -> Result<String, String> {
    // Every request here is https only, and a site that still names itself
    // over http in the spec nearly always serves the same listing over https.
    let upgraded = source
        .strip_prefix("http://")
        .map(|rest| format!("https://{rest}"));
    let source = upgraded.as_deref().unwrap_or(source);
    let file = last_segment(source);
    let (name, packaged) = archive_name_version(file)
        .ok_or_else(|| format!("cannot read a project name from the file name in {source}"))?;
    // A release directory per version (`.../mariadb-13.0.2/source/mariadb-13.0.2.tar.gz`)
    // lists only its own release: the newer ones are siblings of that directory.
    if let Some((base, prefix)) = versioned_directory(source, &packaged) {
        let links = fetch_index(&index_dir(&base))?;
        if let Some(newest) = newest_release_directory(&links, &prefix) {
            return Ok(newest);
        }
    }
    let links = match sourceforge_project(source) {
        Some(project) => sourceforge_files(&project)?,
        None => fetch_index(&index_dir(parent_dir(source)))?,
    };
    newest_for(&links, &name).ok_or_else(|| format!("no release of {name} listed for {source}"))
}

/// When the URL has a path segment that is the packaged version, alone
/// (`1.2.3`), with a `v` (`v1.2.3`) or behind the project's prefix
/// (`mariadb-13.0.2`), the directory above it and that prefix (`mariadb-`).
/// The segment is matched ignoring case; the version may not be the first path
/// segment after the host.
fn versioned_directory(source: &str, version: &str) -> Option<(String, String)> {
    let path = source.split(['?', '#']).next().unwrap_or(source);
    let (scheme, rest) = path.split_once("://")?;
    let segments: Vec<&str> = rest.split('/').collect();
    let version = version.trim_start_matches(['v', 'V']);
    // Host first, file last: only the directories between count.
    for i in 1..segments.len().saturating_sub(1) {
        let segment = segments[i];
        let Some(cut) = segment.len().checked_sub(version.len()) else {
            continue;
        };
        let (Some(prefix), Some(tail)) = (segment.get(..cut), segment.get(cut..)) else {
            continue;
        };
        if !tail.eq_ignore_ascii_case(version) {
            continue;
        }
        if matches!(prefix, "" | "v" | "V") || (prefix.len() > 1 && prefix.ends_with(['-', '_'])) {
            return Some((
                format!("{scheme}://{}", segments[..i].join("/")),
                prefix.to_string(),
            ));
        }
    }
    None
}

/// The highest version among the directories `<prefix><version>/` of a
/// listing; pre-releases only when there is nothing else. Directories that are
/// not versions (`latest`, `stable`, `current`) are ignored.
fn newest_release_directory(links: &[String], prefix: &str) -> Option<String> {
    let mut versions: Vec<String> = links
        .iter()
        .map(|l| l.trim_end_matches('/'))
        .filter(|l| {
            l.get(..prefix.len())
                .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
        })
        .filter_map(|l| l.get(prefix.len()..))
        .filter_map(crate::sources::release_version)
        .collect();
    let stable: Vec<String> = versions
        .iter()
        .filter(|v| !is_prerelease(v))
        .cloned()
        .collect();
    if !stable.is_empty() {
        versions = stable;
    }
    versions.into_iter().max_by(|a, b| version_cmp(a, b))
}

/// The directory a file URL sits in: `https://h/pub/foo-1.tar.gz` is
/// `https://h/pub`. A query or fragment is not part of it.
fn parent_dir(url: &str) -> &str {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    path.trim_end_matches('/')
        .rsplit_once('/')
        .map_or(path, |(dir, _)| dir)
}

/// The highest version among the archives in `links` named `name`; the
/// pre-releases only when there is nothing else.
fn newest_for(links: &[String], name: &str) -> Option<String> {
    let norm = |s: &str| s.replace('_', "-").to_ascii_lowercase();
    let mut versions: Vec<String> = links
        .iter()
        .map(|l| last_segment(l))
        .filter(|f| is_any_archive(f))
        .filter_map(archive_name_version)
        .filter(|(n, _)| norm(n) == norm(name))
        .filter_map(|(_, v)| crate::sources::release_version(&v))
        .collect();
    let stable: Vec<String> = versions
        .iter()
        .filter(|v| !is_prerelease(v))
        .cloned()
        .collect();
    if !stable.is_empty() {
        versions = stable;
    }
    versions.into_iter().max_by(|a, b| version_cmp(a, b))
}

/// The SourceForge project a download URL belongs to.
///
/// `sourceforge.net/projects/<p>/files/…`, `downloads.sourceforge.net/project/<p>/…`
/// and `prdownloads.sourceforge.net/<p>/…` all name it.
pub fn sourceforge_project(url: &str) -> Option<String> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let (host, path) = rest.split_once('/')?;
    let mut segments = path.split('/').filter(|s| !s.is_empty());
    let project = match host {
        "sourceforge.net" | "www.sourceforge.net" => {
            (segments.next()? == "projects").then(|| segments.next())??
        }
        "downloads.sourceforge.net" | "download.sourceforge.net" => {
            let first = segments.next()?;
            if first == "project" {
                segments.next()?
            } else {
                first
            }
        }
        "prdownloads.sourceforge.net" => segments.next()?,
        _ => return None,
    };
    Some(project.to_string())
}

/// The file paths a SourceForge project's feed lists.
fn sourceforge_files(project: &str) -> Result<Vec<String>, String> {
    let url = format!("https://sourceforge.net/projects/{project}/rss?path=/");
    Ok(rss_titles(&get_text(&url)?))
}

/// The `<title>` of every item of an RSS document.
fn rss_titles(rss: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = rss;
    while let Some(i) = rest.find("<title>") {
        let after = &rest[i + "<title>".len()..];
        let Some(end) = after.find("</title>") else {
            break;
        };
        let title = after[..end]
            .trim()
            .trim_start_matches("<![CDATA[")
            .trim_end_matches("]]>")
            .trim();
        out.push(title.to_string());
        rest = &after[end..];
    }
    out
}

fn get_text(url: &str) -> Result<String, String> {
    let resp = ap_ecosystem::net::call_with_retries(|| {
        crate::http::get(url).header("User-Agent", USER_AGENT)
    })
    .map_err(|e| format!("{url}: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("{url}: HTTP {}", resp.status()));
    }
    crate::http::read_text(resp).map_err(|e| format!("{url}: {e}"))
}

/// The newest version of `name` that Repology records, taken from the entries
/// it marks as the newest. A `python-` name is also tried as Repology's
/// `python:` project.
pub fn latest_on_repology(name: &str) -> Result<String, String> {
    let mut tried = vec![name.to_string()];
    if let Some(rest) = name.strip_prefix("python-") {
        tried.push(format!("python:{rest}"));
    }
    let mut last = String::new();
    for project in tried {
        let url = format!(
            "https://repology.org/api/v1/project/{}",
            urlencoding::encode(&project)
        );
        if !space_out_repology() {
            return Err("interrupted".to_string());
        }
        match get_text(&url).and_then(|body| newest_on_repology(&body)) {
            Ok(v) => return Ok(v),
            Err(e) => last = e,
        }
    }
    Err(last)
}

/// Wait for this request's turn, one `REPOLOGY_SPACING` after the previous
/// one. The turn is reserved under the lock and the wait happens outside it,
/// in short steps that notice a shutdown, so a second caller never sits on the
/// mutex behind a sleeper. `false` when the run was interrupted meanwhile.
fn space_out_repology() -> bool {
    static LAST: Mutex<Option<Instant>> = Mutex::new(None);
    let now = Instant::now();
    let turn = {
        let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
        let turn = next_turn(*last, now, REPOLOGY_SPACING);
        *last = Some(turn);
        turn
    };
    crate::osc::sleep_interruptible(turn.saturating_duration_since(now));
    !crate::proc::shutting_down()
}

/// When the next request may go out: now, or `spacing` after the one before it
/// (itself possibly still in the future, when callers queue up).
fn next_turn(last: Option<Instant>, now: Instant, spacing: Duration) -> Instant {
    last.map_or(now, |at| now.max(at + spacing))
}

/// The highest version among the entries of a Repology project that are
/// marked `newest`.
fn newest_on_repology(body: &str) -> Result<String, String> {
    let entries: Vec<serde_json::Value> =
        serde_json::from_str(body).map_err(|e| format!("Repology sent no list: {e}"))?;
    entries
        .iter()
        .filter(|e| e["status"] == "newest")
        .filter_map(|e| e["version"].as_str())
        .max_by(|a, b| version_cmp(a, b))
        .map(str::to_string)
        .ok_or_else(|| "Repology has no newest version for it".to_string())
}

/// The newest version rubygems.org lists for `gem`, from its latest-version
/// document. The host must pass the shared allowlist for source fetches.
pub fn latest_on_rubygems(gem: &str) -> Result<String, String> {
    let valid = !gem.is_empty()
        && gem
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if !valid {
        return Err(format!("{gem:?} is not a gem name"));
    }
    let host = "rubygems.org";
    crate::allowed_hosts::shared()
        .check_host(
            host,
            crate::allowed_hosts::Use::Source,
            &crate::allowed_hosts::Asker::source(gem),
        )
        .map_err(|e| format!("{host}: {e}"))?;
    let url = format!("https://{host}/api/v1/versions/{gem}/latest.json");
    newest_on_rubygems(&get_text(&url)?)
}

/// The `version` a rubygems.org latest-version document names.
fn newest_on_rubygems(body: &str) -> Result<String, String> {
    let doc: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("rubygems.org sent no JSON: {e}"))?;
    doc["version"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| "rubygems.org names no version for it".to_string())
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_directory_per_release_is_searched_beside_itself() {
        assert_eq!(
            versioned_directory(
                "https://archive.mariadb.org/mariadb-13.0.2/source/mariadb-13.0.2.tar.gz",
                "13.0.2"
            ),
            Some((
                "https://archive.mariadb.org".to_string(),
                "mariadb-".to_string()
            ))
        );
        assert_eq!(
            versioned_directory("https://h/pub/foo-1.2.tar.gz", "1.2"),
            None
        );
        let links: Vec<String> = [
            "mariadb-13.0.2/",
            "mariadb-13.1.0/",
            "mariadb-13.1/",
            "mariadb-13.2.0/",
            "foo/",
            "mariadb-14.0.0-rc1/",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(
            newest_release_directory(&links, "mariadb-").as_deref(),
            Some("13.2.0")
        );
    }

    #[test]
    fn a_source_marker_or_a_platform_build_is_not_the_release() {
        let l: Vec<String> = [
            "collectl-4.3.8.src.tar.gz",
            "collectl-4.3.7.src.tar.gz",
            "libircclient-1.10.tar.gz",
            "libircclient-1.10-win32-dll-openssl.tar.gz",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(newest_for(&l, "collectl").as_deref(), Some("4.3.8"));
        assert_eq!(newest_for(&l, "libircclient").as_deref(), Some("1.10"));
        assert_eq!(
            crate::sources::release_version("3.9.5-src").as_deref(),
            Some("3.9.5")
        );
        assert_eq!(
            crate::sources::release_version("2.1.13-stable").as_deref(),
            Some("2.1.13-stable")
        );
    }
    use super::*;

    fn links(files: &[&str]) -> Vec<String> {
        files.iter().map(|f| f.to_string()).collect()
    }

    #[test]
    fn the_newest_archive_of_the_named_project_wins_whatever_its_compression() {
        let l = links(&[
            "acl-2.3.1.tar.gz",
            "acl-2.3.2.tar.xz",
            "acl-2.10.0.tar.xz",
            "attr-9.9.9.tar.xz",
            "acl-2.3.2.tar.xz.sig",
            "README",
        ]);
        assert_eq!(newest_for(&l, "acl").as_deref(), Some("2.10.0"));
        assert_eq!(newest_for(&l, "nothing"), None);
    }

    #[test]
    fn the_listing_read_is_the_directory_of_the_file_not_the_file() {
        assert_eq!(
            parent_dir("https://ftp.gnu.org/gnu/bc/bc-1.08.2.tar.gz"),
            "https://ftp.gnu.org/gnu/bc"
        );
        assert_eq!(
            parent_dir("https://h/pub/a.tar.gz?x=1#frag"),
            "https://h/pub"
        );
        assert_eq!(
            index_dir(parent_dir("https://h/pub/a.tar.gz")),
            "https://h/pub/"
        );
    }

    #[test]
    fn a_prerelease_is_chosen_only_when_it_is_all_there_is() {
        let l = links(&["foo-2.0.0-rc1.tar.gz", "foo-1.9.tar.gz"]);
        assert_eq!(newest_for(&l, "foo").as_deref(), Some("1.9"));
        let only = links(&["foo-2.0.0-rc1.tar.gz"]);
        assert_eq!(newest_for(&only, "foo").as_deref(), Some("2.0.0-rc1"));
    }

    #[test]
    fn sourceforge_projects_are_read_from_each_url_shape() {
        for url in [
            "https://sourceforge.net/projects/acpiclient/files/acpiclient/2.3/acpi-2.3.tar.gz",
            "https://downloads.sourceforge.net/project/acpiclient/acpiclient/2.3/acpi-2.3.tar.gz",
            "https://prdownloads.sourceforge.net/acpiclient/acpi-2.3.tar.gz",
        ] {
            assert_eq!(
                sourceforge_project(url).as_deref(),
                Some("acpiclient"),
                "{url}"
            );
        }
        assert_eq!(
            sourceforge_project("https://ftp.gnu.org/gnu/acl/acl-1.tar.gz"),
            None
        );
    }

    #[test]
    fn rss_titles_are_unwrapped() {
        let rss = "<rss><channel><title>Files</title><item><title><![CDATA[/a/acpi-2.3.tar.gz]]></title></item></channel></rss>";
        assert_eq!(rss_titles(rss), ["Files", "/a/acpi-2.3.tar.gz"]);
    }

    #[test]
    fn repology_requests_queue_one_spacing_apart() {
        let spacing = Duration::from_secs(1);
        let t0 = Instant::now();
        // The first goes at once; one right behind it waits out the spacing.
        assert_eq!(next_turn(None, t0, spacing), t0);
        assert_eq!(next_turn(Some(t0), t0, spacing), t0 + spacing);
        // Callers that arrive together get consecutive turns, not the same one.
        let second = next_turn(Some(t0), t0, spacing);
        assert_eq!(next_turn(Some(second), t0, spacing), t0 + 2 * spacing);
        // After a long pause there is nothing to wait for.
        let later = t0 + Duration::from_secs(60);
        assert_eq!(next_turn(Some(t0), later, spacing), later);
    }

    #[test]
    fn repology_gives_the_highest_newest_version() {
        let body = r#"[
            {"repo":"a","version":"2.3.1","status":"newest"},
            {"repo":"b","version":"2.10.0","status":"newest"},
            {"repo":"c","version":"2.2","status":"outdated"}
        ]"#;
        assert_eq!(newest_on_repology(body).as_deref(), Ok("2.10.0"));
        assert!(newest_on_repology("[]").is_err());
        assert!(newest_on_repology("not json").is_err());
    }

    #[test]
    fn rubygems_names_the_version_of_its_latest_document() {
        assert_eq!(
            newest_on_rubygems(r#"{"version":"6.1.0","authors":"x"}"#).as_deref(),
            Ok("6.1.0")
        );
        assert!(newest_on_rubygems(r#"{"name":"x"}"#).is_err());
        assert!(newest_on_rubygems("<html>").is_err());
        // A malformed gem name is refused before anything is fetched.
        assert!(latest_on_rubygems("../etc").is_err());
        assert!(latest_on_rubygems("").is_err());
    }

    #[test]
    fn a_versioned_directory_is_found_in_each_host_layout() {
        let cases = [
            // `v` directories
            (
                "https://dl.example.org/pkg/v1.2.3/foo-1.2.3.tar.gz",
                "https://dl.example.org/pkg",
                "v",
            ),
            (
                "https://dl.example.org/pkg/V1.2.3/foo-1.2.3.tar.gz",
                "https://dl.example.org/pkg",
                "V",
            ),
            // bare version directory
            (
                "https://dl.example.org/foo/1.2.3/foo-1.2.3.tar.gz",
                "https://dl.example.org/foo",
                "",
            ),
            // upper-case spelling of the version
            (
                "https://dl.example.org/foo/Foo-1.2.3/foo-1.2.3.tar.gz",
                "https://dl.example.org/foo",
                "Foo-",
            ),
        ];
        for (url, base, prefix) in cases {
            assert_eq!(
                versioned_directory(url, "1.2.3"),
                Some((base.to_string(), prefix.to_string())),
                "{url}"
            );
        }
        assert_eq!(
            versioned_directory("https://ftp.gnu.org/gnu/foo/foo-1.2.3.tar.gz", "1.2.3"),
            None
        );
    }

    #[test]
    fn directories_that_are_not_releases_are_ignored() {
        let l = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let links = l(&[
            "latest/",
            "stable/",
            "current",
            "v1.9.0/",
            "v1.10.0/",
            "v2.0.0-rc1/",
            "v1.10.1.tar.gz",
            "v1.8.0/",
        ]);
        assert_eq!(
            newest_release_directory(&links, "v").as_deref(),
            Some("1.10.0")
        );
        let links = l(&["1.2/", "1.10/", "Latest/", "2.0-beta1/", "../"]);
        assert_eq!(
            newest_release_directory(&links, "").as_deref(),
            Some("1.10")
        );
        let links = l(&["Foo-1.0/", "foo-1.1/", "foo-latest/"]);
        assert_eq!(
            newest_release_directory(&links, "foo-").as_deref(),
            Some("1.1")
        );
        // Only pre-releases: they are all there is.
        assert_eq!(
            newest_release_directory(&l(&["v2.0-rc1/", "v2.0-rc2/"]), "v").as_deref(),
            Some("2.0-rc2")
        );
    }
}
