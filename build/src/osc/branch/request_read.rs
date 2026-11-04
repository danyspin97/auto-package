//! Reads for the review of a submit request: the request document, its source
//! diff, package listings at a revision, one file's bytes and the open
//! requests to a package. Nothing here changes OBS: the only call that is not
//! a GET is the diff, which `osc rdiff` sends too and which changes nothing on
//! the server.
//!
//! Every path comes from a pure function with its own test; the wrappers need a
//! live OBS and are not covered.

use std::path::{Path, PathBuf};

use super::*;
use crate::osc::error::*;
use crate::osc::files::is_safe_file_arg;
use crate::osc::names::*;
use crate::osc::query::*;

/// The time the source diff may take, as `osc rdiff` is given.
const DIFF_TIMEOUT: Duration = Duration::from_secs(180);

/// The path of request `id`, or `None` when `id` is not all digits.
pub fn request_path(id: &str) -> Option<String> {
    (!id.is_empty() && id.bytes().all(|b| b.is_ascii_digit())).then(|| format!("/request/{id}"))
}

/// The XML document of request `id`.
pub fn request_document(id: &str, emit: Emit<'_>) -> Result<String, Error> {
    let path = request_path(id)
        .ok_or_else(|| Error::NotActionable(format!("{id:?} is not a request id (digits only)")))?;
    api_get_within(&path, emit, OSC_QUERY_TIMEOUT)
}

/// What a source diff compares: the source package at `rev` against the
/// target's current package.
#[derive(Debug, Clone, Copy)]
pub struct DiffSpec<'a> {
    pub source_project: &'a str,
    pub source_package: &'a str,
    pub rev: Option<&'a str>,
    pub target_project: &'a str,
    pub target_package: &'a str,
    /// The target has no such package yet: compare against nothing.
    pub target_missing: bool,
}

/// The diff call of `osc rdiff`: expanded, unified, with the target named as
/// the old side.
pub fn source_diff_path(d: &DiffSpec<'_>) -> String {
    let rev = d.rev.map_or(String::new(), |r| format!("&rev={}", enc(r)));
    let missing = if d.target_missing { "&missingok=1" } else { "" };
    format!(
        "/source/{}/{}?cmd=diff&oproject={}&opackage={}{rev}&expand=1&unified=1{missing}",
        d.source_project,
        enc(d.source_package),
        enc(d.target_project),
        enc(d.target_package),
    )
}

/// The unified diff of the source against the target.
pub fn source_diff(d: &DiffSpec<'_>, emit: Emit<'_>) -> Result<String, Error> {
    check_obs_name("project", d.source_project)?;
    check_obs_name("package", d.source_package)?;
    check_obs_name("project", d.target_project)?;
    check_obs_name("package", d.target_package)?;
    let path = source_diff_path(d);
    let out = api_call_within(Some("POST"), &path, None, emit, DIFF_TIMEOUT)?;
    if !out.success() {
        return Err(Error::OscFailed {
            cmd: format!("api -X POST {path}"),
            code: out.code,
            stderr: out.stderr,
        });
    }
    Ok(out.stdout)
}

/// One file of a package listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedFile {
    pub name: String,
    pub md5: String,
    pub size: u64,
}

/// A package listing: the revision's `srcmd5` and its files.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Listing {
    pub srcmd5: Option<String>,
    pub files: Vec<ListedFile>,
}

/// The listing in a `<directory>` document. An entry without a name is skipped.
pub fn parse_listing(xml: &str) -> Listing {
    let files = xml
        .split("<entry ")
        .skip(1)
        .filter_map(|e| {
            let tag = &e[..e.find('>').unwrap_or(e.len())];
            Some(ListedFile {
                name: crate::obs_xml::attr(tag, "name").filter(|n| !n.is_empty())?,
                md5: crate::obs_xml::attr(tag, "md5").unwrap_or_default(),
                size: crate::obs_xml::attr(tag, "size")
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0),
            })
        })
        .collect();
    Listing {
        srcmd5: parse_directory_srcmd5(xml),
        files,
    }
}

/// The path of one file of a package, expanded like [`listing_path`]: in a link
/// package a file that comes from the base is listed by the expanded listing
/// and is only served by the expanded form.
pub fn file_path(project: &str, package: &str, file: &str, rev: Option<&str>) -> String {
    let rev = rev.map_or(String::new(), |r| format!("rev={}&", enc(r)));
    format!(
        "/source/{project}/{}/{}?{rev}expand=1",
        enc(package),
        enc(file)
    )
}

/// The text of one file of `project/package` at `rev` (current without one),
/// expanded.
pub fn cat_file_expanded(
    project: &str,
    package: &str,
    file: &str,
    rev: Option<&str>,
    emit: Emit<'_>,
) -> Result<String, Error> {
    check_obs_name("project", project)?;
    check_obs_name("package", package)?;
    if !is_safe_file_arg(file) {
        return Err(Error::NotActionable(format!("{file:?} is not a file name")));
    }
    api_get_within(
        &file_path(project, package, file, rev),
        emit,
        OSC_QUERY_TIMEOUT,
    )
}

/// The listing path of a package, expanded, at `rev` or at its current state.
pub fn listing_path(project: &str, package: &str, rev: Option<&str>) -> String {
    let rev = rev.map_or(String::new(), |r| format!("rev={}&", enc(r)));
    format!("/source/{project}/{}?{rev}expand=1", enc(package))
}

/// The files of `project/package` at `rev` (its current state without one).
/// `Ok(None)` when OBS says the package or revision is not there.
pub fn package_entries_at_revision(
    project: &str,
    package: &str,
    rev: Option<&str>,
    emit: Emit<'_>,
) -> Result<Option<Listing>, Error> {
    check_obs_name("project", project)?;
    check_obs_name("package", package)?;
    match api_get_within(
        &listing_path(project, package, rev),
        emit,
        OSC_QUERY_TIMEOUT,
    ) {
        Ok(xml) => Ok(Some(parse_listing(&xml))),
        Err(Error::OscFailed { stderr, .. }) if is_not_found(&stderr) => Ok(None),
        Err(e) => Err(e),
    }
}

/// Fetch one file at `rev` into `dir` with `osc co`, so a binary archive is
/// written by osc itself. Returns where the file is.
pub fn fetch_file_at_revision(
    project: &str,
    package: &str,
    file: &str,
    rev: &str,
    dir: &Path,
    emit: Emit<'_>,
) -> Result<PathBuf, Error> {
    check_obs_name("project", project)?;
    check_obs_name("package", package)?;
    if !is_safe_file_arg(file) || rev.starts_with('-') {
        return Err(Error::NotActionable(format!(
            "refusing to fetch {file:?} at {rev:?}"
        )));
    }
    let out = proc::capture_with_timeout(
        "osc",
        // -e: a file of a link package that the base provides is only there expanded.
        &["co", "-e", "-r", rev, project, package, file],
        dir,
        emit,
        OSC_TRANSFER_TIMEOUT,
    )
    .map_err(during(format!("co {project}/{package} {file}")))?;
    if !out.success() {
        return Err(Error::OscFailed {
            cmd: format!("co {project}/{package} {file}"),
            code: out.code,
            stderr: out.stderr,
        });
    }
    // With a file named, osc writes it into the directory it runs in.
    Ok(dir.join(file))
}

/// The query for the open submit requests whose target is
/// `project/package`.
pub fn open_requests_to_path(project: &str, package: &str) -> String {
    format!(
        "/request?view=collection&roles=target&types=submit&states=new,review&project={}&package={}",
        enc(project),
        enc(package)
    )
}

/// The open submit requests to `project/package`.
pub fn open_requests_to(
    project: &str,
    package: &str,
    emit: Emit<'_>,
) -> Result<Vec<OpenRequest>, Error> {
    check_obs_name("project", project)?;
    check_obs_name("package", package)?;
    let xml = api_get_within(
        &open_requests_to_path(project, package),
        emit,
        OSC_QUERY_TIMEOUT,
    )?;
    Ok(parse_open_requests(&xml))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_request_path_refuses_an_id_that_is_not_digits() {
        assert_eq!(request_path("1394662").as_deref(), Some("/request/1394662"));
        assert_eq!(request_path("12a"), None);
        assert_eq!(request_path(""), None);
        assert_eq!(request_path("1/../x"), None);
    }

    fn spec<'a>(missing: bool) -> DiffSpec<'a> {
        DiffSpec {
            source_project: "home:u:branches:Base:System",
            source_package: "libsigc++",
            rev: Some("3"),
            target_project: "Base:System",
            target_package: "libsigc++",
            target_missing: missing,
        }
    }

    #[test]
    fn the_diff_path_names_the_revision_and_the_target_encoded() {
        let path = source_diff_path(&spec(false));
        assert_eq!(
            path,
            "/source/home:u:branches:Base:System/libsigc%2B%2B?cmd=diff&oproject=Base%3ASystem\
             &opackage=libsigc%2B%2B&rev=3&expand=1&unified=1"
        );
    }

    #[test]
    fn the_diff_path_of_a_new_package_allows_a_missing_target() {
        assert!(source_diff_path(&spec(true)).ends_with("&missingok=1"));
        assert!(!source_diff_path(&spec(false)).contains("missingok"));
    }

    #[test]
    fn the_listing_reads_name_md5_and_size_and_skips_an_unnamed_entry() {
        let xml = r#"<directory name="p" rev="3" srcmd5="abc123">
  <entry name="a.spec" md5="11" size="120" mtime="1"/>
  <entry md5="22" size="5"/>
  <entry name="a.tar.bz2" md5="33" size="7340032"/>
</directory>"#;
        let l = parse_listing(xml);
        assert_eq!(l.srcmd5.as_deref(), Some("abc123"));
        assert_eq!(
            l.files,
            vec![
                ListedFile {
                    name: "a.spec".into(),
                    md5: "11".into(),
                    size: 120
                },
                ListedFile {
                    name: "a.tar.bz2".into(),
                    md5: "33".into(),
                    size: 7340032
                },
            ]
        );
    }

    #[test]
    fn the_listing_path_expands_and_carries_the_revision_when_given() {
        assert_eq!(
            listing_path("Base:System", "cracklib", Some("3")),
            "/source/Base:System/cracklib?rev=3&expand=1"
        );
        assert_eq!(
            listing_path("Base:System", "cracklib", None),
            "/source/Base:System/cracklib?expand=1"
        );
    }

    #[test]
    fn a_file_is_read_expanded_so_a_files_the_base_provides_is_found() {
        assert_eq!(
            file_path("Base:System", "libsigc++", "a b.spec", Some("3")),
            "/source/Base:System/libsigc%2B%2B/a%20b.spec?rev=3&expand=1"
        );
        assert_eq!(
            file_path("Base:System", "p", "p.spec", None),
            "/source/Base:System/p/p.spec?expand=1"
        );
        assert!(
            cat_file_expanded("Base:System", "p", "../x", None, crate::proc::Emit::quiet())
                .is_err()
        );
    }

    #[test]
    fn the_target_collection_asks_for_the_target_role() {
        let p = open_requests_to_path("Base:System", "libsigc++");
        assert!(p.contains("roles=target"));
        assert!(p.contains("package=libsigc%2B%2B"));
        assert!(p.contains("project=Base%3ASystem"));
    }

    #[test]
    fn a_fetch_refuses_a_file_name_that_leaves_the_package() {
        let dir = std::env::temp_dir();
        for bad in ["../x", "-x", "a/b", ""] {
            let r = fetch_file_at_revision(
                "Base:System",
                "p",
                bad,
                "3",
                &dir,
                crate::proc::Emit::quiet(),
            );
            assert!(r.is_err(), "{bad:?}");
        }
    }
}
