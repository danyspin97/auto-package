//! The XML of a `_service` that supplies a package's source: which service does it,
//! where its revision is, and what `Version:` an `obs_scm` one gives a tag.

use super::*;

pub(super) fn refuse_manual_vendoring(masked: &str) -> Result<(), String> {
    for name in ["cargo_vendor", "node_modules", "go_modules"] {
        for (at, _) in masked.match_indices(&format!("name=\"{name}\"")) {
            if crate::obs_xml::attr(open_tag(masked, at), "mode").as_deref() != Some("buildtime") {
                return Err(format!(
                    "the _service also has a `{name}` service that is not mode=\"buildtime\", \
                     so its archive would stay the old one at build time; nothing was changed"
                ));
            }
        }
    }
    Ok(())
}

/// Refuses unless the tag gives `version` when OBS builds the package. `Ok(Some(why))`
/// when that cannot be worked out here: the update is accepted with `why` as its
/// note, since an empty or unknown version must not refuse it.
pub(super) fn check_build_time_version(
    params: &[(String, String)],
    tag: &str,
    version: &str,
) -> Result<Option<String>, String> {
    let got = match build_time_version(params, tag) {
        Ok(got) if !got.is_empty() => got,
        Ok(_) => {
            return Ok(Some(format!(
                "the tag `{tag}` gives an empty Version at build time, so the spec's Version was not checked"
            )));
        }
        Err(why) => {
            return Ok(Some(format!(
                "the Version the tag `{tag}` gives at build time cannot be worked out here ({why}), \
                 so the spec's Version was not checked"
            )));
        }
    };
    if got == version {
        return Ok(None);
    }
    Err(format!(
        "the tag `{tag}` gives Version `{got}` when OBS builds the package, not `{version}`, \
         so the spec would name a version the source does not have; nothing was changed"
    ))
}

/// The `Version` that an `obs_scm` service with these `params` gives the tag
/// `tag` when OBS builds it (TarSCM's `get_version`): the `versionformat` with
/// `@PARENT_TAG@` the tag and `@TAG_OFFSET@` 0 (the tag is the revision), then
/// `versionrewrite-pattern` with `versionrewrite-replacement` (`\1` by default),
/// or without a pattern the `-` and `:` removed; then `versionprefix` as
/// `prefix.version`. `version` other than `_auto_` is used as it is unless a
/// `versionformat` is set. `Err` when the result cannot be worked out here.
pub(super) fn build_time_version(params: &[(String, String)], tag: &str) -> Result<String, String> {
    let param = |name: &str| param_value(params, name);
    let nonempty = |name: &str| param(name).filter(|v| !v.is_empty());
    if param("scm").is_some_and(|scm| scm != "git") {
        return Err("only a git source is worked out here".into());
    }
    let version = match param("version") {
        Some("_none_") => {
            return Err("the _service sets version to _none_, which keeps the spec's".into());
        }
        Some(given) if given != "_auto_" && nonempty("versionformat").is_none() => {
            given.to_string()
        }
        _ => {
            // OBS takes the parent tag from these, not from the revision: the
            // version they give is not the one this tag gives.
            if param("parent-tag").is_some() || param("match-tag").is_some() {
                return Err(
                    "parent-tag or match-tag is set, which picks the tag @PARENT_TAG@ and \
                     @TAG_OFFSET@ expand to"
                        .into(),
                );
            }
            detected_version(nonempty("versionformat"), tag)?
        }
    };
    let version = match nonempty("versionrewrite-pattern") {
        Some(pattern) => {
            let re = regex::Regex::new(pattern)
                .map_err(|e| format!("versionrewrite-pattern `{pattern}`: {e}"))?;
            let replacement =
                regex_replacement(param("versionrewrite-replacement").unwrap_or("\\1"))?;
            re.replace_all(&version, replacement.as_str()).into_owned()
        }
        None => version.replace(['-', ':'], ""),
    };
    Ok(match nonempty("versionprefix") {
        Some(prefix) => format!("{prefix}.{version}"),
        None => version,
    })
}

/// The version `versionformat` gives the tag, before any rewrite. Without a
/// format OBS uses the commit time, and git format codes (`%ct`) are not
/// expanded here.
fn detected_version(format: Option<&str>, tag: &str) -> Result<String, String> {
    let Some(format) = format else {
        return Err("the default versionformat is the commit time".into());
    };
    if format.contains('%') {
        return Err(format!("versionformat `{format}` has git format codes"));
    }
    let version = format
        .replace("@PARENT_TAG@", tag)
        .replace("@TAG_OFFSET@", "0");
    if version.contains('@') {
        return Err(format!(
            "versionformat `{format}` has a macro that is not known"
        ));
    }
    Ok(version)
}

/// Whether a `revision` value is a placeholder (`@PARENT_TAG@`) that OBS expands
/// when it builds the package, not a tag name.
fn is_placeholder(revision: &str) -> bool {
    let v = revision.trim();
    v.len() > 2 && v.starts_with('@') && v.ends_with('@')
}

/// An OBS rewrite replacement in the syntax of the `regex` crate (`${1}`).
/// OBS expands it as Python's `re.sub` does: `\<digit>` (1 to 9) is a group and
/// `$` is literal. Any other escape (`\g<1>`, `\10`, `\0`, `\n`, `\\`) is
/// `Err`: it is not worked out here.
pub(super) fn regex_replacement(osc: &str) -> Result<String, String> {
    let unsupported = |escape: &str| {
        format!("the replacement `{osc}` has the escape {escape}, which is not worked out here")
    };
    let mut out = String::new();
    let mut chars = osc.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '$' => out.push_str("$$"),
            '\\' => match chars.next() {
                Some(d @ '1'..='9') if !chars.peek().is_some_and(|n| n.is_ascii_digit()) => {
                    out.push_str(&format!("${{{d}}}"));
                }
                Some(d) => return Err(unsupported(&format!("\\{d}"))),
                None => return Err(unsupported("at the end")),
            },
            c => out.push(c),
        }
    }
    Ok(out)
}

/// Whether the checkout's `_service` gets the source from an `obs_scm` service.
/// OBS makes that source when the package is built: no archive is in the
/// checkout and nothing is to run here.
pub(super) fn source_made_at_build_time(checkout_dir: &Path) -> bool {
    std::fs::read_to_string(checkout_dir.join("_service"))
        .is_ok_and(|s| service_makes_source_at_build_time(&s))
}

/// Whether the `_service` text `service` gives the source from an `obs_scm`
/// service. An empty text (no `_service`) gives none.
pub(super) fn service_makes_source_at_build_time(service: &str) -> bool {
    matches!(
        source_service(&mask_xml_comments(service)),
        Some((_, SourceService::BuildTime))
    )
}

/// The `_service` text of the checkout at `checkout_dir`; empty without one.
pub(super) fn checkout_service(checkout_dir: &Path) -> String {
    std::fs::read_to_string(checkout_dir.join("_service")).unwrap_or_default()
}

/// The sources the expanded spec names that are missing from the checkout and
/// are not made at build time (see `still_missing`).
pub(super) fn missing_at_checkout(expanded: &str, checkout_dir: &Path) -> Vec<String> {
    still_missing(
        missing_source_files(expanded, checkout_dir),
        &checkout_service(checkout_dir),
    )
}

/// The `missing` sources that the `_service` text `service` makes at build
/// time, which are not missing: OBS runs the buildtime services when it builds
/// the package. Only an `obs_scm` source is considered (see
/// `made_at_build_time` for the names it can make). Anything else stays missing.
pub(super) fn still_missing(missing: Vec<String>, service: &str) -> Vec<String> {
    let masked = mask_xml_comments(service);
    let Some((scm, SourceService::BuildTime)) = source_service(&masked) else {
        return missing;
    };
    missing
        .into_iter()
        .filter(|n| !made_at_build_time(n, service, &masked, scm))
        .collect()
}

/// Whether OBS makes the archive `name` at build time, for the `obs_scm` source
/// at `scm` (an offset in `masked`). The names are:
/// - `<filename>-<version>.tar`, or `.tar.<c>` where `recompress` gives `c`,
///   when a `tar` service is present. `filename` is the `filename` parameter,
///   or the name TarSCM gives the checkout (see `default_filename_of`); the
///   `<version>` is whatever the name carries.
/// - `vendor.tar.<c>` from a buildtime `cargo_vendor` service (method
///   `vendor`, compression `c`, default `zst`; `not` gives no suffix).
fn made_at_build_time(name: &str, service: &str, masked: &str, scm: usize) -> bool {
    let params = source_params(service, masked, scm);
    let packed = !service_elements(masked, "tar").is_empty();
    let mut suffixes: Vec<&str> = Vec::new();
    if packed {
        suffixes.push(".tar");
        for at in service_elements(masked, "recompress") {
            let params = source_params(service, masked, at);
            suffixes.extend(param_value(&params, "compression").and_then(recompressed_suffix));
        }
    }
    let archive = packed
        && default_filename_of(&params).is_some_and(|stem| {
            name.strip_prefix(stem.as_str())
                .and_then(|rest| rest.strip_prefix('-'))
                .is_some_and(|rest| {
                    suffixes.iter().any(|s| {
                        rest.strip_suffix(s)
                            .is_some_and(|v| !v.is_empty() && !v.contains('/'))
                    })
                })
        });
    archive
        || service_elements(masked, "cargo_vendor")
            .into_iter()
            .any(|at| {
                let params = source_params(service, masked, at);
                crate::obs_xml::attr(open_tag(masked, at), "mode").as_deref() == Some("buildtime")
                    && param_value(&params, "method").unwrap_or("vendor") == "vendor"
                    && name
                        == format!(
                            "vendor.tar{}",
                            vendor_suffix(param_value(&params, "compression").unwrap_or("zst"))
                        )
            })
}

/// The `filename` the `obs_scm` source gives its archives: its `filename`
/// parameter when set, else the name TarSCM gives the checkout (the last part
/// of the `url`, without the `scm` suffix).
fn default_filename_of(params: &[(String, String)]) -> Option<String> {
    param_value(params, "filename")
        .filter(|f| !f.is_empty())
        .map(str::to_string)
        .or_else(|| {
            let url = param_value(params, "url")?;
            let scm = param_value(params, "scm").unwrap_or("git");
            let path = url.split(['?', '#']).next()?.trim_end_matches('/');
            let last = path.rsplit(['/', ':']).next()?;
            let stem = last.strip_suffix(&format!(".{scm}")).unwrap_or(last);
            (!stem.is_empty()).then(|| stem.to_string())
        })
}

/// The archive suffix of a `recompress` compression (`xz`, `gz`, `bz2`, `zst`).
fn recompressed_suffix(compression: &str) -> Option<&'static str> {
    match compression {
        "xz" => Some(".tar.xz"),
        "gz" | "gzip" => Some(".tar.gz"),
        "bz2" => Some(".tar.bz2"),
        "zst" => Some(".tar.zst"),
        _ => None,
    }
}

/// The suffix of the `vendor.tar` that a `cargo_vendor` compression gives.
fn vendor_suffix(compression: &str) -> &'static str {
    match compression {
        "gz" => ".gz",
        "xz" => ".xz",
        "bz2" => ".bz2",
        "not" => "",
        _ => ".zst",
    }
}

/// Whether `name` ends like a source archive.
pub(super) fn is_archive_name(name: &str) -> bool {
    [".tar.gz", ".tgz", ".tar.bz2", ".tar.xz", ".tar.zst", ".tar"]
        .iter()
        .any(|s| name.ends_with(s))
}

/// The tarballs (by file name) in `dir`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SourceService {
    /// `tar_scm`, or `cargo_vendor` with a `url`: the source is fetched here.
    Fetched,
    /// `obs_scm`: OBS makes the source at build time.
    BuildTime,
}

/// Where the service that supplies the source starts in `masked` (the offset
/// of its name, comments blanked) and how it gets the source. The `tar_scm`
/// service comes first, then a `cargo_vendor` one that has a `url` parameter
/// (it fetches the source and vendors it in one go; without a `url` it only
/// vendors), then `obs_scm`.
pub(super) fn source_service(masked: &str) -> Option<(usize, SourceService)> {
    if let Some(at) = masked.find("name=\"tar_scm\"") {
        return Some((at, SourceService::Fetched));
    }
    if let Some(at) = masked.find("name=\"cargo_vendor\"")
        && service_block(masked, at).contains("<param name=\"url\">")
    {
        return Some((at, SourceService::Fetched));
    }
    masked
        .find("name=\"obs_scm\"")
        .map(|at| (at, SourceService::BuildTime))
}

/// Where the value of the `revision` parameter of the source service at `scm`
/// starts and ends, in `masked`'s offsets.
pub(super) fn revision_span(masked: &str, scm: usize) -> Option<(usize, usize)> {
    let open = "<param name=\"revision\">";
    let block = service_block(masked, scm);
    let start = block.find(open)? + open.len();
    let end = start + block[start..].find("</param>")?;
    Some((scm + start, scm + end))
}

/// `service` with the `revision` parameter of the service that supplies the
/// source set to `tag`; `None` when it has none. A placeholder revision
/// (`@PARENT_TAG@`) is kept as it is.
pub(super) fn with_service_revision(service: &str, tag: &str) -> Option<String> {
    let masked = mask_xml_comments(service);
    let (scm, _) = source_service(&masked)?;
    let (start, end) = revision_span(&masked, scm)?;
    if is_placeholder(&service[start..end]) {
        // OBS expands the placeholder to the newest tag when it builds the
        // package; moving it to one tag would pin the source to that tag.
        return Some(service.to_string());
    }
    Some(format!(
        "{}{}{}",
        &service[..start],
        crate::obs_xml::escape(tag),
        &service[end..]
    ))
}

/// Why the `Version` a build-time tag gives cannot be checked here, when it
/// cannot: `parent-tag` or `match-tag` (OBS picks the tag from another one), or
/// a source that is not git. A placeholder revision is not among them: OBS builds
/// the newest tag then, and the tag looked up is checked as for any carry.
pub(super) fn build_time_unverifiable(params: &[(String, String)]) -> Option<String> {
    if let Some(scm) = param_value(params, "scm").filter(|scm| *scm != "git") {
        return Some(format!("the source is `{scm}`, not git"));
    }
    if param_value(params, "parent-tag").is_some() || param_value(params, "match-tag").is_some() {
        return Some("parent-tag or match-tag picks the tag OBS builds, not the revision".into());
    }
    None
}

/// `service` with the `versionformat` of the `obs_scm` service at `scm` set to
/// `@PARENT_TAG@` when it is a literal version. A literal names every tag the
/// same way, so moving the revision alone would keep the old version. The
/// rewrite is made only when the literal is the spec's current `Version`
/// (`old_version`) exactly, since that is the version the tag gave before. A
/// literal that is not (`1.0.3+git`), or any literal when `unverifiable` says
/// the new version cannot be checked, is `Err` and nothing is changed. Any
/// other value (empty, or with a macro or a git format code) is kept.
pub(super) fn with_parent_tag_version_format(
    service: &str,
    masked: &str,
    scm: usize,
    old_version: Option<&str>,
    unverifiable: Option<&str>,
) -> Result<String, String> {
    let open = "<param name=\"versionformat\">";
    let block = service_block(masked, scm);
    let Some(start) = block.find(open).map(|i| scm + i + open.len()) else {
        return Ok(service.to_string());
    };
    let Some(len) = masked[start..].find("</param>") else {
        return Ok(service.to_string());
    };
    let end = start + len;
    let value = crate::obs_xml::unescape(service[start..end].trim());
    if value.is_empty() || value.contains(['@', '%']) {
        return Ok(service.to_string());
    }
    if let Some(why) = unverifiable {
        return Err(format!(
            "{why}, and the literal versionformat `{value}` would keep the old version at build time; nothing was changed"
        ));
    }
    if old_version != Some(value.as_str()) {
        return Err(format!(
            "the literal versionformat `{value}` is not the spec's Version{}, so it cannot follow the tag; nothing was changed",
            old_version.map(|v| format!(" `{v}`")).unwrap_or_default()
        ));
    }
    Ok(format!(
        "{}@PARENT_TAG@{}",
        &service[..start],
        &service[end..]
    ))
}
