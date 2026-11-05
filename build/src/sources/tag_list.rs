//! The tags a forge repository lists, read a page at a time, and the version a
//! forge answer gives from them.

use super::*;

/// Tags per page of a forge's tag listing.
const TAG_PAGE: usize = 100;
/// Pages read at most: a repository with more tags than this is not listed
/// whole, so its newest version is not known.
const TAG_PAGES: u32 = 5;

/// The tag-listing endpoint of a forge project, chosen the same way as
/// [`release_endpoints`]: its first page.
pub(super) fn tag_list_url(host: &str, project_path: &str) -> String {
    if is_github_host(host) {
        format!("https://api.github.com/repos/{project_path}/tags?per_page={TAG_PAGE}")
    } else if is_gitlab_host(host) {
        let encoded = project_path.replace('/', "%2F");
        format!("https://{host}/api/v4/projects/{encoded}/repository/tags?per_page={TAG_PAGE}")
    } else {
        // Gitea/Forgejo
        format!("https://{host}/api/v1/repos/{project_path}/tags?limit={TAG_PAGE}")
    }
}

/// The tag-listing endpoint of a forge project for page `page` (1 is the first).
fn tag_page_url(host: &str, project_path: &str, page: u32) -> String {
    let base = tag_list_url(host, project_path);
    if page == 1 {
        base
    } else {
        format!("{base}&page={page}")
    }
}

/// The tags a repository lists, and whether that is all of them.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct TagListing {
    pub(super) tags: Vec<String>,
    /// False when the list was cut short: more tags than were read, or a page
    /// after the first failed.
    pub(super) complete: bool,
}

/// The first page of a repository's tags, as the forge lists them: one request,
/// so a list longer than a page is partial. Callers that need only the
/// repository's newest tags for a lookup use this.
pub(super) fn get_repo_tags(repo_url: &str) -> Result<Vec<String>, String> {
    if source_kind(repo_url) != SourceKind::Forge {
        return Ok(Vec::new());
    }
    let (host, project_path, _repo_name) = owner_repo(repo_url)?;
    if is_sourcehut_host(&host) {
        return ls_remote_tags(&format!("https://{host}/{project_path}"));
    }
    first_page(|page| read_page(&host, &project_path, page))
}

/// Every tag a repository lists, page by page (see [`read_tag_pages`]).
pub(super) fn get_repo_tag_listing(repo_url: &str) -> Result<TagListing, String> {
    if source_kind(repo_url) != SourceKind::Forge {
        return Ok(TagListing {
            tags: Vec::new(),
            complete: true,
        });
    }
    let (host, project_path, _repo_name) = owner_repo(repo_url)?;
    if is_sourcehut_host(&host) {
        return Ok(TagListing {
            tags: ls_remote_tags(&format!("https://{host}/{project_path}"))?,
            complete: true,
        });
    }
    read_tag_pages(|page| read_page(&host, &project_path, page))
}

/// One page of a forge's tag listing: its tag names, and how many entries the
/// page held.
fn read_page(host: &str, project_path: &str, page: u32) -> Result<(Vec<String>, usize), String> {
    let json = get_json(&tag_page_url(host, project_path, page))?;
    let arr = json.as_array().cloned().unwrap_or_default();
    let names = arr
        .iter()
        .filter_map(|item| {
            item.get("name")
                .or_else(|| item.get("tag_name"))
                .and_then(|t| t.as_str())
                .map(str::to_string)
        })
        .collect();
    Ok((names, arr.len()))
}

/// The names of the first page `page(1)` reads.
fn first_page(
    page: impl Fn(u32) -> Result<(Vec<String>, usize), String>,
) -> Result<Vec<String>, String> {
    page(1).map(|(names, _)| names)
}

/// Every tag of a repository, read page by page from `page(n)`, which gives the
/// names on page `n` and how many entries the page held. A page is read while
/// the one before was full. The page after [`TAG_PAGES`] full ones is probed: an
/// empty answer there makes the list complete. A failure on the first page is an
/// error; one on a later page keeps the pages read, as an incomplete listing.
pub(super) fn read_tag_pages(
    page: impl Fn(u32) -> Result<(Vec<String>, usize), String>,
) -> Result<TagListing, String> {
    let mut tags = Vec::new();
    for n in 1..=TAG_PAGES + 1 {
        match page(n) {
            Ok((names, entries)) if n > TAG_PAGES => {
                return Ok(TagListing {
                    tags,
                    complete: entries == 0 && names.is_empty(),
                });
            }
            Ok((names, entries)) => {
                tags.extend(names);
                if entries < TAG_PAGE {
                    return Ok(TagListing {
                        tags,
                        complete: true,
                    });
                }
            }
            Err(why) if n == 1 => return Err(why),
            Err(_) => {
                return Ok(TagListing {
                    tags,
                    complete: false,
                });
            }
        }
    }
    unreachable!("the loop returns on its last page")
}

/// The version a forge answer gives for its first tag `tag`: the tag's own
/// version when it names one, else the newest version among the repository's
/// tags. `Ok(None)` when the listing has no version either: a tag that marks no
/// release (`latest`). A listing that was cut short is an error: its newest
/// version is not known.
pub(super) fn version_for_tag_or_listing(
    tag: &str,
    repo: &str,
    listed: impl FnOnce() -> Result<TagListing, String>,
) -> Result<Option<String>, String> {
    let version = version_from_tag(tag, repo);
    if version.chars().any(|c| c.is_ascii_digit()) {
        return Ok(Some(version));
    }
    let listing = listed()?;
    if !listing.complete {
        return Err(format!(
            "{repo}: its tag list is truncated, so its newest version must be checked by hand"
        ));
    }
    Ok(newest_version(&listing.tags, repo))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn full(names: &[&str]) -> Result<(Vec<String>, usize), String> {
        Ok((names.iter().map(|t| t.to_string()).collect(), names.len()))
    }

    fn hundred() -> Result<(Vec<String>, usize), String> {
        Ok((vec!["v1.0".to_string(); TAG_PAGE], TAG_PAGE))
    }

    #[test]
    fn a_caller_of_the_first_page_gets_its_partial_answer() {
        // A repository with more tags than a page: the first page is the answer.
        let got = first_page(|n| {
            if n == 1 {
                hundred()
            } else {
                panic!("one request")
            }
        });
        assert_eq!(got.map(|t| t.len()), Ok(TAG_PAGE));
    }

    #[test]
    fn a_short_page_ends_the_listing_complete() {
        let got = read_tag_pages(|n| match n {
            1 => hundred(),
            _ => full(&["v2.0"]),
        });
        assert_eq!(
            got,
            Ok(TagListing {
                tags: [vec!["v1.0".to_string(); TAG_PAGE], vec!["v2.0".to_string()]].concat(),
                complete: true,
            })
        );
    }

    #[test]
    fn exactly_five_full_pages_are_complete_when_the_probe_is_empty() {
        let got = read_tag_pages(|n| if n <= TAG_PAGES { hundred() } else { full(&[]) });
        let listing = got.expect("a complete listing");
        assert_eq!(listing.tags.len(), TAG_PAGE * TAG_PAGES as usize);
        assert!(listing.complete);
    }

    #[test]
    fn more_than_five_full_pages_are_incomplete() {
        let got = read_tag_pages(|_| hundred());
        let listing = got.expect("the pages read are kept");
        assert_eq!(listing.tags.len(), TAG_PAGE * TAG_PAGES as usize);
        assert!(!listing.complete);
    }

    #[test]
    fn a_failed_later_page_keeps_the_pages_read_as_incomplete() {
        let got = read_tag_pages(|n| {
            if n == 1 {
                hundred()
            } else {
                Err("HTTP 503".into())
            }
        });
        assert_eq!(
            got,
            Ok(TagListing {
                tags: vec!["v1.0".to_string(); TAG_PAGE],
                complete: false,
            })
        );
    }

    #[test]
    fn a_failed_first_page_is_an_error() {
        assert_eq!(
            read_tag_pages(|_| Err("HTTP 503".to_string())),
            Err("HTTP 503".to_string())
        );
    }

    #[test]
    fn the_latest_fallback_on_a_truncated_listing_is_an_error() {
        let truncated = || {
            Ok(TagListing {
                tags: vec!["v1.9.0".into(), "latest".into()],
                complete: false,
            })
        };
        let why = version_for_tag_or_listing("latest", "imaptest", truncated)
            .expect_err("a truncated listing must not give a newest version");
        assert!(why.contains("truncated"), "{why}");
        let complete = || {
            Ok(TagListing {
                tags: vec!["v1.9.0".into(), "v1.10.0".into(), "latest".into()],
                complete: true,
            })
        };
        assert_eq!(
            version_for_tag_or_listing("latest", "imaptest", complete),
            Ok(Some("1.10.0".to_string()))
        );
    }

    #[test]
    fn the_first_page_keeps_the_plain_endpoint() {
        assert_eq!(
            tag_page_url("github.com", "o/r", 1),
            "https://api.github.com/repos/o/r/tags?per_page=100"
        );
        assert!(tag_page_url("github.com", "o/r", 2).ends_with("&page=2"));
    }
}
