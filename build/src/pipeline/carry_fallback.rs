//! A carried-forward source that cannot be downloaded: the same release is
//! looked for under the forge's own spelling of its tag, then on PyPI.

use super::*;
use crate::http::FetchFailure;

/// Whether a failed download of the spec's first source may be retried at
/// another address. Only [`FetchFailure::NotFound`] (HTTP 404: the address
/// does not name an archive) qualifies. A rate limit, a transport error or a
/// full disk keeps the error the caller reports.
pub(super) fn may_fall_back(failure: &FetchFailure) -> bool {
    *failure == FetchFailure::NotFound
}

/// The index of the line `set_first_source` rewrites: the first `Source:` or
/// `SourceN:` line of `spec`.
pub(super) fn first_source_line(spec: &str) -> Option<usize> {
    spec.lines().position(|line| {
        line.strip_prefix("Source").is_some_and(|rest| {
            let digits = rest.chars().take_while(char::is_ascii_digit).count();
            rest[digits..].starts_with(':')
        })
    })
}

/// `file` (a source's file name with its macros already expanded) with the
/// package name and the version written back as `%{name}` and `%{version}`,
/// so that the spec's `#/` rename expands to the name the file has on disk.
pub(super) fn macro_file_name(file: &str, name: &str, version: &str) -> String {
    let mut out = String::new();
    let mut rest = file;
    while !rest.is_empty() {
        if !name.is_empty() && rest.starts_with(name) {
            out.push_str("%{name}");
            rest = &rest[name.len()..];
        } else if !version.is_empty() && rest.starts_with(version) {
            out.push_str("%{version}");
            rest = &rest[version.len()..];
        } else {
            let c = rest.chars().next().unwrap_or_default();
            out.push(c);
            rest = &rest[c.len_utf8()..];
        }
    }
    out
}

/// What to do about the failed download `error` of source `index` of the
/// carried spec `bumped`. Only the 404 of the spec's first Source: line is
/// retried elsewhere, see [`fetch_first_source_elsewhere`]; any other failure
/// gives `Err` with nothing to append, so the caller reports the error as it
/// is.
pub(super) fn after_failed_download(
    config: &PipelineConfig,
    bumped: &str,
    index: usize,
    source: &crate::spec::SourceEntry,
    dest: &Path,
    error: &FetchFailure,
) -> Result<(String, &'static str), String> {
    if !may_retry(bumped, index, error) {
        return Err(String::new());
    }
    fetch_first_source_elsewhere(config, bumped, &source.url, &source.file, dest)
}

/// Whether source `index` (in [`crate::spec::expanded_sources`] order) is the
/// spec's first Source: line and its download failed with `error`.
fn may_retry(bumped: &str, index: usize, error: &FetchFailure) -> bool {
    let Some(first) = first_source_line(bumped) else {
        return false;
    };
    crate::spec::expanded_source_lines(bumped)
        .get(index)
        .is_some_and(|(at, _)| *at == first)
        && may_fall_back(error)
}

/// Downloads the first source of a carried spec to `dest` when the address the
/// spec gives for the new version does not answer. `file` is the source's file
/// name, the one `dest` has. Tries the forge's tag for the version (a `v`
/// prefix, another spelling), then the PyPI sdist.
///
/// `Ok` is the spec text with its first `Source:` moved to the address that
/// worked, and the `.changes` bullet that says so; the caller adds the bullet
/// once the spec is written. `Err` is the text to append to the download error:
/// empty when there was no other address to try, else what was tried.
fn fetch_first_source_elsewhere(
    config: &PipelineConfig,
    bumped: &str,
    failed_url: &str,
    file: &str,
    dest: &Path,
) -> Result<(String, &'static str), String> {
    let mut tried: Vec<String> = Vec::new();
    let mut candidates: Vec<(String, &'static str)> = Vec::new();
    if let Some(repo) = config.source_repo.as_deref()
        && let Some(url) = crate::sources::tag_archive_url(repo, &config.version)
        && url != failed_url
    {
        candidates.push((url, "Fetch the source from the upstream tag of the version"));
    }
    let pypi = config.pkg_name.to_lowercase();
    if let Some(first) = pypi.chars().next() {
        let url = format!(
            "https://files.pythonhosted.org/packages/source/{first}/{pypi}/{pypi}-{}.tar.gz",
            config.version
        );
        // PyPI is the last resort: it must have the release before it is tried.
        if matches!(
            fetch_pypi_json(&config.pkg_name, &config.version),
            Ok(Some(_))
        ) {
            candidates.push((
                url,
                "Fetch the source from PyPI, as upstream has no tag for it",
            ));
        }
    }
    // The spec's `#/` rename is the name the file has on disk, with the name
    // and version as macros so the spec keeps following the version.
    let name = crate::spec::parse_header(bumped)
        .map(|h| h.name)
        .unwrap_or_default();
    let rename = macro_file_name(file, &name, &config.version);
    for (url, bullet) in candidates {
        match crate::sources::fetch_archive_to(&url, dest) {
            Ok(()) => {
                let value = if url.contains("files.pythonhosted.org") {
                    let first = pypi.chars().next().unwrap_or('x');
                    format!(
                        "https://files.pythonhosted.org/packages/source/{first}/{pypi}/{pypi}-%{{version}}.tar.gz#/{rename}"
                    )
                } else {
                    format!("{url}#/{rename}")
                };
                let moved = crate::spec::set_first_source(bumped, &value)
                    .ok_or_else(|| "; the spec has no Source: line".to_string())?;
                return Ok((moved, bullet));
            }
            Err(e) => tried.push(format!("{url}: {e}")),
        }
    }
    Err(if tried.is_empty() {
        String::new()
    } else {
        format!("; also tried {}", tried.join("; "))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_keeps_the_name_and_version_as_macros() {
        assert_eq!(
            macro_file_name("foo-1.2.tar.gz", "foo", "1.2"),
            "%{name}-%{version}.tar.gz"
        );
        assert_eq!(
            macro_file_name("v1.2.tar.gz", "foo", "1.2"),
            "v%{version}.tar.gz"
        );
        assert_eq!(
            macro_file_name("Foo-1.2.tar.gz", "foo", "1.2"),
            "Foo-%{version}.tar.gz"
        );
        assert_eq!(macro_file_name("x.tar.gz", "", ""), "x.tar.gz");
    }

    #[test]
    fn a_name_inserted_as_a_macro_is_not_read_again_for_the_version() {
        assert_eq!(
            macro_file_name("e-e.tar.gz", "e", "e"),
            "%{name}-%{name}.tar.gz"
        );
    }

    #[test]
    fn the_first_source_line_is_the_one_set_first_source_rewrites() {
        let spec = "Name: foo\nPatch0: p.patch\nSource1: b.tar.gz\nSource0: https://x/a.tar.gz\n";
        assert_eq!(first_source_line(spec), Some(2));
        let rewritten = crate::spec::set_first_source(spec, "https://y/z.tar.gz").unwrap();
        let changed: Vec<usize> = spec
            .lines()
            .zip(rewritten.lines())
            .enumerate()
            .filter(|(_, (a, b))| a != b)
            .map(|(i, _)| i)
            .collect();
        assert_eq!(changed, [2]);
        assert_eq!(first_source_line("Name: foo\nPatch0: p\n"), None);
        assert_eq!(first_source_line("Source: a\nSource0: b\n"), Some(0));
    }

    #[test]
    fn only_the_first_source_line_with_a_not_found_answer_may_retry() {
        // The first Source: line is `Source0:`, the web address of source 0.
        let spec =
            "Name: foo\nVersion: 1.0\nSource0: https://x/a.tar.gz\nSource1: https://x/b.tar.gz\n";
        let not_found = FetchFailure::NotFound;
        assert!(may_retry(spec, 0, &not_found));
        assert!(!may_retry(spec, 1, &not_found));
        assert!(!may_retry(spec, 0, &FetchFailure::RateLimited));
        // A first Source: line that is not a web address is not a source to
        // fetch, so the web address after it is not the first line.
        let keyed = "Name: foo\nVersion: 1.0\nSource0: foo.keyring\nSource1: https://x/a.tar.gz\n";
        assert!(!may_retry(keyed, 0, &not_found));
        assert!(!may_retry("Name: foo\nVersion: 1.0\n", 0, &not_found));
    }

    #[test]
    fn only_a_not_found_answer_may_fall_back() {
        assert!(may_fall_back(&FetchFailure::NotFound));
        assert!(!may_fall_back(&FetchFailure::RateLimited));
        assert!(!may_fall_back(&FetchFailure::Transport("timed out".into())));
        assert!(!may_fall_back(&FetchFailure::TooLarge));
    }
}
