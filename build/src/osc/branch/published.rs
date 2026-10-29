//! Published binaries: the binary records of a package, the search path of a
//! name, and the parsing and searching of the published list.

use super::*;

/// A binary OBS has published, as `/search/published/binary/id` reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishedBinary {
    pub name: String,
    pub project: String,
    pub repository: String,
    pub arch: String,
    /// The source package it was built from.
    pub package: Option<String>,
    pub version: Option<String>,
}

/// Whether `name` can be put in a published-binary search at all. A name with
/// a quote or a backslash cannot be written into the query, so it is never
/// searched, and an answer about it is unknown rather than "nothing publishes
/// it".
pub fn is_searchable_binary_name(name: &str) -> bool {
    !name.is_empty() && !name.contains(['\'', '"', '\\'])
}

/// The search request for published binaries named any of `names`, across
/// every project. Names [`is_searchable_binary_name`] rejects are left out.
pub fn published_binary_search_path(names: &[&str]) -> String {
    let terms: Vec<String> = names
        .iter()
        .filter(|n| is_searchable_binary_name(n))
        .map(|n| format!("@name='{n}'"))
        .collect();
    format!(
        "/search/published/binary/id?match={}",
        enc(&terms.join(" or "))
    )
}

/// Parsing half of [`search_published_binaries`].
pub fn parse_published_binaries(xml: &str) -> Vec<PublishedBinary> {
    xml.split("<binary ")
        .skip(1)
        .filter_map(|chunk| {
            let tag = &chunk[..chunk.find('>').unwrap_or(chunk.len())];
            Some(PublishedBinary {
                name: crate::obs_xml::attr(tag, "name")?,
                project: crate::obs_xml::attr(tag, "project")?,
                repository: crate::obs_xml::attr(tag, "repository")?,
                arch: crate::obs_xml::attr(tag, "arch")?,
                package: crate::obs_xml::attr(tag, "package"),
                version: crate::obs_xml::attr(tag, "version"),
            })
        })
        .collect()
}

/// Every published binary named any of `names`, in any project.
pub fn search_published_binaries(
    names: &[&str],
    emit: Emit<'_>,
) -> Result<Vec<PublishedBinary>, Error> {
    if names.iter().all(|n| !is_searchable_binary_name(n)) {
        return Ok(Vec::new());
    }
    let xml = api_get(&published_binary_search_path(names), emit)?;
    Ok(parse_published_binaries(&xml))
}
