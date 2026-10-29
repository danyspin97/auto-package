//! Published binaries: the cached lists per project, the name matches and the
//! respelled names that a project ships.

use super::*;

/// What a published-binary search matches a name by.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum NameMatch {
    Exactly,
    Containing,
}

/// The OBS `match` expression for binaries published by one of `projects`
/// (any repository: the `snapshot` repository a project builds against is not
/// searchable, but the `standard` one it is made from is) for `arch`.
pub(super) fn published_binary_match(
    projects: &[String],
    name: &str,
    by: NameMatch,
    arch: &str,
) -> String {
    let quote = |s: &str| s.replace('\'', "");
    let name_clause = match by {
        NameMatch::Exactly => format!("@name='{}'", quote(name)),
        NameMatch::Containing => format!("contains(@name,'{}')", quote(name)),
    };
    let projects = projects
        .iter()
        .map(|p| format!("@project='{}'", quote(p)))
        .collect::<Vec<_>>()
        .join(" or ");
    // A pure-Python (or any other noarch) RPM is published as `arch="noarch"`, so
    // `@arch='x86_64'` alone never finds it: every noarch package looked "not
    // shipped", and `python313-huggingface_hub` was queued as a new package while
    // `python313-huggingface-hub` was published.
    format!(
        "{name_clause} and ({projects}) and (@arch='{0}' or @arch='noarch')",
        quote(arch)
    )
}

/// The distinct binary names in a `/search/published/binary/id` answer.
pub(super) fn parse_published_binary_names(xml: &str) -> Vec<String> {
    let mut names: Vec<String> = xml
        .split("<binary ")
        .skip(1)
        .filter_map(|chunk| {
            crate::obs_xml::attr(&chunk[..chunk.find('>').unwrap_or(chunk.len())], "name")
        })
        .collect();
    names.sort();
    names.dedup();
    names
}

pub(super) static PUBLISHED_BINARIES_CACHE: std::sync::Mutex<Option<HashMap<String, Vec<String>>>> =
    std::sync::Mutex::new(None);

/// Bumped by every [`forget_published_binaries`]. A search that started before
/// an invalidation may have read the projects before the change; its answer is
/// stored only if the generation is still the one it started under.
static PUBLISHED_BINARIES_GENERATION: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

pub(super) fn published_binaries_generation() -> u64 {
    PUBLISHED_BINARIES_GENERATION.load(std::sync::atomic::Ordering::SeqCst)
}

/// Store `names` for `expr` unless the cache was invalidated since
/// `generation` was read. Checked under the cache lock, like
/// `insert_if_current`, and invalidation bumps the generation under the same
/// lock, so a stale answer cannot land after the removal.
pub(super) fn insert_published_if_current(expr: String, generation: u64, names: Vec<String>) {
    let mut guard = PUBLISHED_BINARIES_CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if published_binaries_generation() == generation {
        guard.get_or_insert_with(HashMap::new).insert(expr, names);
    }
}

/// The names of binaries the projects `project` builds against publish for
/// `arch` that match `name`. `None` when the search could not be made (no
/// answer is not "nothing published").
pub(super) fn published_binaries(
    project: &str,
    arch: &str,
    name: &str,
    by: NameMatch,
    emit: Emit<'_>,
) -> Option<Vec<String>> {
    let generation = published_binaries_generation();
    let paths = get_project_repo_paths(project, emit).ok()?;
    let mut projects: Vec<String> = paths.into_iter().map(|(p, _)| p).collect();
    projects.sort();
    projects.dedup();
    if projects.is_empty() {
        return None;
    }
    let expr = published_binary_match(&projects, name, by, arch);
    if let Some(hit) = PUBLISHED_BINARIES_CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .and_then(|c| c.get(&expr).cloned())
    {
        return Some(hit);
    }
    let xml = osc_api(&format!("/search/published/binary/id?match={}", enc(&expr)))?;
    // A document without a collection is not an answer.
    if !xml.contains("<collection") {
        return None;
    }
    let names = parse_published_binary_names(&xml);
    insert_published_if_current(expr, generation, names.clone());
    Some(names)
}

/// Whether a project `project` builds against publishes a binary package called
/// exactly `name` for `arch`. This is what a build there can install, which the
/// host's own repositories (Packman, a different snapshot) are not. `None`
/// when the search failed.
pub fn project_ships_binary(project: &str, arch: &str, name: &str, emit: Emit<'_>) -> Option<bool> {
    if name.trim().is_empty() || name.contains('\'') {
        return Some(false);
    }
    published_binaries(project, arch, name, NameMatch::Exactly, emit).map(|n| !n.is_empty())
}

/// Whether `name` is something the projects `project` builds against provide.
/// A package name is asked of what those projects publish, which is what a
/// build there can install; the host's repositories (Packman's ffmpeg, another
/// snapshot) say nothing about it. A capability (a soname, `pkgconfig(x)`) is
/// not a binary name, so the host answers it, as does a failed search.
pub fn available_to_project(project: &str, arch: &str, name: &str, emit: Emit<'_>) -> bool {
    if is_rpm_capability(name) {
        return distro_provides(name);
    }
    match project_ships_binary(project, arch, name, emit) {
        Some(shipped) => shipped,
        None => distro_provides(name),
    }
}

/// Binaries those projects publish with a name that merely contains the stem of
/// `name` (`ffmpeg-8-devel` -> `ffmpeg-8-mini-devel`, `ffmpeg-8-libavcodec-devel`):
/// what exists when the exact name does not. Empty when nothing is similar or
/// the search failed. `name` itself and debug packages are left out.
pub fn similar_published_binaries(
    project: &str,
    arch: &str,
    name: &str,
    emit: Emit<'_>,
) -> Vec<String> {
    let stem = name
        .trim_end_matches("-devel-static")
        .trim_end_matches("-devel");
    if stem.len() < 3 || stem.contains('\'') {
        return Vec::new();
    }
    let mut found: Vec<String> =
        published_binaries(project, arch, stem, NameMatch::Containing, emit).unwrap_or_default();
    found.retain(|n| {
        n != name && !n.ends_with("-debuginfo") && !n.ends_with("-debugsource") && n.contains(stem)
    });
    // The names with the same suffix as the missing one come first.
    let wants_devel = name.ends_with("-devel");
    found.sort_by_key(|n| (n.ends_with("-devel") != wants_devel, n.len()));
    found.truncate(12);
    found
}

/// The spelling the projects publish for what `name` (an RPM name such as
/// `python313-huggingface_hub`) asks for, when it is spelled differently: RPM
/// names do not follow PyPI's rule, so the distro has `huggingface-hub` but
/// `typing_extensions`, and a spec copying the PyPI name can be wrong either
/// way. The answer is the bare name, without the `python313-` prefix.
pub fn published_respelling(
    project: &str,
    arch: &str,
    name: &str,
    emit: Emit<'_>,
) -> Option<String> {
    // The search matches substrings, not across separators: ask for the
    // longest run the spellings must share.
    let term = strip_rpm_prefix(name)
        .split(['-', '_', '.'])
        .max_by_key(|part| part.len())
        .filter(|part| part.len() >= 3 && !part.contains('\''))?;
    // `contains()` is case-sensitive and the distro spells some names with capitals
    // (`python313-Brotli`, asked for as `brotli`): look for the usual variants too.
    let mut cased: Vec<String> = Vec::new();
    let mut chars = term.chars();
    let capitalized: String = chars
        .next()
        .map(|c| c.to_uppercase().chain(chars).collect())
        .unwrap_or_default();
    for variant in [
        term.to_string(),
        term.to_lowercase(),
        capitalized,
        term.to_uppercase(),
    ] {
        if !cased.contains(&variant) {
            cased.push(variant);
        }
    }
    let mut published: Vec<String> = Vec::new();
    let mut answered = false;
    for variant in &cased {
        if let Some(names) = published_binaries(project, arch, variant, NameMatch::Containing, emit)
        {
            answered = true;
            published.extend(names);
        }
    }
    if !answered {
        return None;
    }
    published.sort();
    published.dedup();
    respelling_among(name, &published)
}

/// The respelling of `name` among `published` binary names, if one is there
/// under the same prefix and the exact spelling is not.
pub(super) fn respelling_among(name: &str, published: &[String]) -> Option<String> {
    let bare = strip_rpm_prefix(name);
    let prefix = &name[..name.len() - bare.len()];
    let key = fuzzy_key(bare);
    if published.iter().any(|p| p == name) {
        return None;
    }
    let mut found: Vec<&str> = published
        .iter()
        .filter_map(|p| p.strip_prefix(prefix))
        .filter(|p| *p != bare && fuzzy_key(p) == key && strip_rpm_prefix(p) == *p)
        .collect();
    found.sort_unstable();
    found.first().map(|p| p.to_string())
}

/// Drop every cached published-binary search. A search is keyed by the expression
/// over the projects `project` builds against, and a project built or published
/// into can be one of those, so the answers are not keyed well enough to drop
/// only the ones that changed. Losing them costs one re-search each.
pub(super) fn forget_published_binaries() {
    let mut guard = PUBLISHED_BINARIES_CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    PUBLISHED_BINARIES_GENERATION.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    *guard = None;
}
