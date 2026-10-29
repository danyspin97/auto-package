//! The Source lines of a spec: parsing them, expanding them, setting the first
//! source and adopting the sources of a tree.

use super::*;
use crate::spec::expand::*;

/// A `SourceN:` entry that names something to download.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceEntry {
    /// The address, macros expanded and the `#/<file>` rename cut off.
    pub url: String,
    /// The file name the source has in the package: the rename when there is
    /// one, the last part of the address otherwise.
    pub file: String,
}

/// A `Source`/`SourceN:` (or `Patch`/`PatchN:`) line split into what it names:
/// the address, and the file name given by a `#/<file>` fragment if there is
/// one. Built by [`parse_source_line`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceLine {
    /// The N of `SourceN:`; a plain `Source:` is 0, as rpm reads it.
    pub index: u32,
    /// The address, with the `#` fragment cut off.
    pub url: String,
    /// The text after the first `#`, without its leading `/`.
    pub rename: Option<String>,
}

impl SourceLine {
    /// The file name the source has in the package: the rename when there is
    /// one, the last part of the address otherwise. Empty when the address
    /// has no last part.
    pub fn file(&self) -> &str {
        match self.rename.as_deref() {
            Some(r) if !r.is_empty() && !r.contains('/') => r,
            _ => self.url.rsplit('/').next().unwrap_or(&self.url),
        }
    }
}

/// Parse one spec line whose tag is one of `tags` followed by optional digits
/// (`source`, `Source0`, `SOURCE12`). The tag is matched without case and
/// without the spaces around it. `expand` gives the value its macros
/// expanded, before it is split at `#`; pass the identity to read it raw.
pub fn parse_source_line(
    line: &str,
    tags: &[&str],
    expand: impl Fn(&str) -> String,
) -> Option<SourceLine> {
    let (tag, value) = line.split_once(':')?;
    let tag = tag.trim().to_ascii_lowercase();
    let digits = tags.iter().find_map(|t| {
        let digits = tag.strip_prefix(t)?;
        digits.chars().all(|c| c.is_ascii_digit()).then_some(digits)
    })?;
    let index = if digits.is_empty() {
        0
    } else {
        digits.parse().unwrap_or(u32::MAX)
    };
    let expanded = expand(value.trim());
    let (url, rename) = match expanded.split_once('#') {
        Some((url, rename)) => (url, Some(rename.trim_start_matches('/').to_string())),
        None => (expanded.as_str(), None),
    };
    Some(SourceLine {
        index,
        url: url.to_string(),
        rename,
    })
}

/// Every `SourceN:` of the spec that is a web address, with `%{name}`,
/// `%{version}`, `%{url}` and the spec's simple defines expanded.
pub fn expanded_sources(spec: &str) -> Vec<SourceEntry> {
    let Some(header) = parse_header(spec) else {
        return Vec::new();
    };
    let mut macros = collect_simple_defines(spec);
    macros.insert("name".to_string(), header.name.clone());
    macros.insert("version".to_string(), header.version.clone());
    if let Some(url) = &header.url {
        macros.insert("url".to_string(), url.clone());
    }
    // Twice: a define may be spelled with another.
    let expand = |value: &str| {
        let once = expand_simple_defines(value, &macros);
        expand_simple_defines(&once, &macros)
    };
    let mut out = Vec::new();
    for line in spec.lines() {
        let Some(source) = parse_source_line(line, &["source"], expand) else {
            continue;
        };
        if !(source.url.starts_with("http://") || source.url.starts_with("https://"))
            || source.url.contains('%')
        {
            continue;
        }
        let file = source.file();
        if file.is_empty() {
            continue;
        }
        out.push(SourceEntry {
            file: file.to_string(),
            url: source.url,
        });
    }
    out
}

/// `spec` with the value of its first `Source`/`SourceN:` line replaced, the tag
/// and its spacing kept. `None` when the spec has no source line.
pub fn set_first_source(spec: &str, value: &str) -> Option<String> {
    let mut done = false;
    let lines: Vec<String> = spec
        .lines()
        .map(|line| {
            if !done && let Some(rest) = line.strip_prefix("Source") {
                let digits = rest.chars().take_while(char::is_ascii_digit).count();
                if rest[digits..].starts_with(':') {
                    let colon = "Source".len() + digits;
                    let after = &line[colon + 1..];
                    let pad = after.len() - after.trim_start().len();
                    done = true;
                    return format!("{}{}{value}", &line[..=colon], &after[..pad]);
                }
            }
            line.to_string()
        })
        .collect();
    done.then(|| lines.join("\n") + if spec.ends_with('\n') { "\n" } else { "" })
}

/// Copy into `checkout` the files of `missing` (names a spec's `Source`/`Patch`
/// lines give) that the unpacked source `tree` already holds, at its top or in
/// its single top-level directory: a lock file (`package-lock.json`) a
/// `node_modules` service reads, a licence, a config the spec ships as its own
/// source. The agent cannot copy them itself (open-webui: `Source10:
/// package-lock.json` stayed missing because the `cp` was refused), and nothing
/// else is needed to fix it. Names with a directory part, symlinks and files
/// over 64 MiB are left alone; an existing file is never overwritten. Returns
/// the names copied.
pub fn adopt_sources_from_tree(
    missing: &[String],
    tree: &std::path::Path,
    checkout: &std::path::Path,
) -> Vec<String> {
    let mut roots = vec![tree.to_path_buf()];
    if let Ok(entries) = std::fs::read_dir(tree) {
        let dirs: Vec<_> = entries
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .map(|e| e.path())
            .collect();
        if dirs.len() == 1 {
            roots.insert(0, dirs[0].clone());
        }
    }
    let mut copied = Vec::new();
    for name in missing {
        if name.is_empty() || name.contains('/') || name.starts_with('.') {
            continue;
        }
        let dest = checkout.join(name);
        if dest.exists() {
            continue;
        }
        let found = roots.iter().map(|r| r.join(name)).find(|c| {
            std::fs::symlink_metadata(c).is_ok_and(|m| m.is_file() && m.len() <= 64 * 1024 * 1024)
        });
        if let Some(src) = found
            && std::fs::copy(&src, &dest).is_ok()
        {
            copied.push(name.clone());
        }
    }
    copied
}
