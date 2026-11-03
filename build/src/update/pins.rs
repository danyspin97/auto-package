//! The series a package is pinned to by its name, and the held-back check that
//! keeps a candidate outside that series.

use super::*;
use std::collections::HashSet;

/// The series a package is pinned to by its name: `fmt-9`, `boost-1.85` and
/// `wlroots-0_15` name `[9]`, `[1, 85]` and `[0, 15]`. Without a dash the
/// number needs an underscore (`pipewire0_2`), or a soname (`libfoo2`) would
/// pin. The number must also start the packaged version, or it is part of the
/// name (`foo-2` packaging 1.4) and pins nothing. A dashed run of digits with
/// no dot is a major and a minor: `mariadb-55` is `[5, 5]` and `mariadb-100`
/// is `[10, 0]`.
pub(super) fn series_of(rpm_name: &str, version: &str) -> Option<Vec<u64>> {
    let tail = rpm_name.trim_end_matches(|c: char| c.is_ascii_digit() || c == '.' || c == '_');
    let suffix = &rpm_name[tail.len()..];
    let dashed = tail.ends_with('-');
    if suffix.is_empty() || suffix.starts_with(['.', '_']) || suffix.ends_with(['.', '_']) {
        return None;
    }
    if !dashed && !suffix.contains('_') {
        return None;
    }
    let parts: Vec<&str> = suffix.split(['.', '_']).collect();
    let series: Vec<u64> = parts.iter().filter_map(|p| p.parse().ok()).collect();
    if series.len() != parts.len() {
        return None;
    }
    let packaged = version::leading_numbers(version);
    if packaged.starts_with(&series) {
        return Some(series);
    }
    if dashed && parts.len() == 1 {
        return major_minor_splits(suffix).find(|s| packaged.starts_with(s));
    }
    None
}

/// The ways a run of digits reads as a major and a minor with no separator:
/// `55` is `[5, 5]`, `100` is `[1, 0]` and `[10, 0]`. A zero-padded part
/// (`100` as `[1, 00]`) is not a number of a series.
fn major_minor_splits(digits: &str) -> impl Iterator<Item = Vec<u64>> + '_ {
    let padded = |p: &str| p.len() > 1 && p.starts_with('0');
    (1..digits.len()).filter_map(move |k| {
        let (major, minor) = digits.split_at(k);
        if padded(major) || padded(minor) {
            return None;
        }
        Some(vec![major.parse().ok()?, minor.parse().ok()?])
    })
}

/// What a pin to the major compares: `[major]`, or `[0, minor]` for 0.x, where
/// every minor is a series of its own (`0.15.2` is `[0, 15]`).
pub(super) fn major_key(version: &str) -> Vec<u64> {
    let numbers = version::leading_numbers(version);
    match numbers.as_slice() {
        [0, minor, ..] => vec![0, *minor],
        [major, ..] => vec![*major],
        [] => Vec::new(),
    }
}

/// The major and minor of a version, which is how a sibling's series is told
/// apart from another (`3.4` and `3.9` are different series of zookeeper).
pub(super) fn series_pair(version: &str) -> Vec<u64> {
    version::leading_numbers(version)
        .into_iter()
        .take(2)
        .collect()
}

/// The series a package's name spells out, when its trailing digits are its
/// major (`influxdb2` on 2.7.10, `gtk3` on 3.24, `nodejs22` on 22.11) or its
/// major and minor written together (`python311` on 3.11.4 is `[3, 11]`,
/// `libpng16` on 1.6.40 is `[1, 6]`). Trailing digits that match neither
/// (`bzip2` on 1.0.8, `lz4` on 1.10) name no series.
pub(super) fn name_series(rpm_name: &str, version: &str) -> Option<Vec<u64>> {
    let digits = &rpm_name[rpm_name
        .trim_end_matches(|c: char| c.is_ascii_digit())
        .len()..];
    if digits.is_empty() {
        return None;
    }
    let numbers = version::leading_numbers(version);
    let major = *numbers.first()?;
    if digits == major.to_string() {
        return Some(major_key(version));
    }
    match numbers.get(1) {
        Some(minor) if digits == format!("{major}{minor}") => Some(vec![major, *minor]),
        _ => None,
    }
}

/// The git forge repository a package's source is, from its `Source:` and
/// `Url:` after the macros are expanded (`https://github.com/influxdata/influxdb`).
fn upstream_of(header: &spec::Header) -> Option<String> {
    [header.expanded_source.as_deref(), header.url.as_deref()]
        .into_iter()
        .flatten()
        .find_map(super::candidates::git_repo_url)
}

/// The project's other packages whose source is the same forge repository as
/// `upstream`, read from their specs: `(name, packaged version)`. This finds a
/// sibling whatever its name (`influxdb2` for `influxdb`, `zookeeper-kit` for
/// `zookeeper`). A sibling whose spec cannot be read says nothing and is left
/// out. Flavors (`boost:base`) are not packages of their own.
fn sibling_versions<SpecLookup>(
    rpm_name: &str,
    upstream: &str,
    project: &HashSet<String>,
    fetch_spec: &SpecLookup,
) -> Vec<(String, String)>
where
    SpecLookup: Fn(&str) -> Result<String, ReadError>,
{
    project
        .iter()
        .filter(|s| s.as_str() != rpm_name && !s.contains(':'))
        .filter_map(|s| {
            let header = spec::parse_header(&fetch_spec(s).ok()?)?;
            (upstream_of(&header).as_deref() == Some(upstream)).then(|| (s.clone(), header.version))
        })
        .collect()
}

/// Whether `rpm_name` is the spec's `name` with a series after it: `mariadb-55`
/// for `mariadb`, `mariadb-100` too, `wlroots-0_16` for `wlroots`. Any other
/// difference (`python-foo` for `foo`, `foo-kit`, a rename) is not a series.
pub(super) fn is_series_copy_of(rpm_name: &str, name: &str) -> bool {
    if name.is_empty() || name.contains('%') {
        return false;
    }
    let Some(rest) = rpm_name.strip_prefix(name) else {
        return false;
    };
    let rest = rest.strip_prefix('-').unwrap_or(rest);
    rest.starts_with(|c: char| c.is_ascii_digit())
        && rest
            .split('_')
            .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
}

/// What holds a package back from a `latest` newer than its packaged version.
pub(super) enum Held {
    /// The package is held to a series: only that series' newest release is
    /// taken, and only a forge repository can say what that is.
    Series(Vec<u64>),
    /// The package is not taken at all; the reason says why.
    Stop(SkipReason),
    /// The package is held to a series or a major that `latest` is already in:
    /// `latest` is taken as it is.
    Pinned,
}

/// Why a `latest` newer than the packaged version is not taken as it is: a
/// package on a series (`fmt-9`, `mariadb-55`), or one pinned to its major
/// because its name is an old series of its spec's Name (`mariadb-55`) or a
/// sibling is on a newer series. A plain package's jump of two majors, or into
/// a series a sibling is on, is left to the maintainer.
pub(super) fn held_back<SpecLookup>(
    rpm_name: &str,
    header: &spec::Header,
    latest: &str,
    fetch_spec: &SpecLookup,
    project: &HashSet<String>,
) -> Option<Held>
where
    SpecLookup: Fn(&str) -> Result<String, ReadError>,
{
    let packaged = header.version.as_str();
    if let Some(series) = series_of(rpm_name, packaged).or_else(|| name_series(rpm_name, packaged))
    {
        return Some(if in_series(latest, &series) {
            Held::Pinned
        } else {
            Held::Series(series)
        });
    }
    let own = major_key(packaged);
    if own.is_empty() {
        return None;
    }
    let own_pair = series_pair(packaged);
    let latest_pair = series_pair(latest);
    let related = match upstream_of(header) {
        Some(upstream) => sibling_versions(rpm_name, &upstream, project, fetch_spec),
        None => Vec::new(),
    };
    // A sibling already on the series this would move to keeps it.
    if let Some((sibling, version)) = related
        .iter()
        .find(|(_, v)| series_pair(v) == latest_pair && series_pair(v) != own_pair)
    {
        return Some(Held::Stop(SkipReason::NeedsMaintainer(format!(
            "{latest} is on the series of {sibling} ({version}), which this package does not follow"
        ))));
    }
    let series_copy = is_series_copy_of(rpm_name, &header.name);
    let newer_sibling = related.iter().any(|(_, v)| series_pair(v) > own_pair);
    if series_copy || newer_sibling {
        return Some(if major_key(latest) != own {
            Held::Series(own)
        } else {
            Held::Pinned
        });
    }
    // A plain package takes a jump of one major; two or more it leaves to the
    // maintainer.
    let first = |v: &str| version::leading_numbers(v).first().copied();
    match (first(packaged), first(latest)) {
        (Some(from), Some(to)) if to >= from + 2 => Some(Held::Stop(SkipReason::NeedsMaintainer(
            format!("{packaged} to {latest} is a jump of {} majors", to - from),
        ))),
        _ => None,
    }
}

/// Whether `version` belongs to `series`: it starts with the same numbers.
pub(super) fn in_series(version: &str, series: &[u64]) -> bool {
    version::leading_numbers(version).starts_with(series)
}
