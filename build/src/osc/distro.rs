//! What the distribution provides: the zypper query of the configured repositories
//! and the cached provides check.

use super::*;

/// Timeout for [`distro_provides`]: a local `zypper search`, not a network
/// round trip to OBS, but `--no-refresh` still has to read each configured
/// repo's on-disk solv cache, which is slow on a cold page cache.
const ZYPPER_QUERY_TIMEOUT: Duration = Duration::from_secs(30);

/// Parse `/etc/zypp/repos.d/*.repo` (plain INI) and return the `baseurl` of
/// every repo whose `[alias]` section does not set `enabled=0`.
///
/// Reads the config files directly instead of `zypper lr`'s table output —
/// that output wraps and column-packs under narrow terminal widths (see
/// `get_project_build_statuses`'s doc comment for a case where exactly this
/// bit a different `osc`/`zypper` table parser), and it's one less process
/// to spawn.
fn configured_repo_urls() -> Vec<(String, String)> {
    let Ok(entries) = fs::read_dir("/etc/zypp/repos.d") else {
        return Vec::new();
    };
    let mut repos = Vec::new();
    for entry in entries.flatten() {
        if entry.path().extension().and_then(|e| e.to_str()) != Some("repo") {
            continue;
        }
        let Ok(contents) = fs::read_to_string(entry.path()) else {
            continue;
        };
        let mut alias: Option<String> = None;
        let mut baseurl: Option<String> = None;
        let mut enabled = true;
        for line in contents.lines() {
            let line = line.trim();
            if let Some(name) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
                if let (Some(a), Some(u)) = (alias.take(), baseurl.take())
                    && enabled
                {
                    repos.push((a, u));
                }
                alias = Some(name.to_string());
                baseurl = None;
                enabled = true;
                continue;
            }
            if let Some(v) = line.strip_prefix("baseurl=") {
                baseurl = Some(v.trim().to_string());
            } else if let Some(v) = line.strip_prefix("enabled=") {
                enabled = v.trim() != "0";
            }
        }
        if let (Some(a), Some(u)) = (alias, baseurl)
            && enabled
        {
            repos.push((a, u));
        }
    }
    repos
}

/// Whether `baseurl` is a genuine openSUSE base-distribution tree rather
/// than a third-party vendor repo (`packman`, `google-cloud-cli`, …) or an
/// arbitrary OBS sub-project (`home:*`, `openSUSE:Tools`, …) that happens to
/// publish a package under a name that collides with what's being searched
/// for. Trusts two shapes:
///
///  - the plain released binary tree, no `/repositories/` segment at all
///    (`download.opensuse.org/tumbleweed/repo/oss/`,
///    `.../update/tumbleweed/`, `.../debug/tumbleweed/repo/oss/`);
///  - the base distro project published *as* an OBS project
///    (`download.opensuse.org/repositories/openSUSE:/Tumbleweed/...` or
///    `.../openSUSE:/Factory/...` or `.../openSUSE:/Leap:...`).
///
/// Any other `/repositories/<namespace>/...` is someone's personal or devel
/// project — never "the distro already has this", no matter how official
/// its packages look.
pub(super) fn is_base_distro_repo(baseurl: &str) -> bool {
    let Some(path) = baseurl
        .strip_prefix("http://download.opensuse.org/")
        .or_else(|| baseurl.strip_prefix("https://download.opensuse.org/"))
    else {
        return false;
    };
    let Some(rest) = path.strip_prefix("repositories/") else {
        // No `/repositories/` segment at all: the plain released tree.
        return true;
    };
    rest.starts_with("openSUSE:/Tumbleweed/")
        || rest.starts_with("openSUSE:/Factory/")
        || rest.starts_with("openSUSE:/Leap:")
}

/// Cache of `(capability -> provided)` for the lifetime of this process.
/// `distro_provides` is called once per reported dependency, repeatedly
/// across rounds for a package stuck reporting the same name, and
/// potentially for the same system capability (`openssl`, `gcc`, …) from
/// several unrelated packages in one run — without this, each repeat spawns
/// its own `zypper` subprocess, up to [`ZYPPER_QUERY_TIMEOUT`] apiece.
static DISTRO_PROVIDES_CACHE: Mutex<Option<HashMap<String, bool>>> = Mutex::new(None);

/// Whether the local system's configured base-distro zypper repositories
/// already provide `capability` — checked with `zypper search --provides
/// --match-exact`, i.e. the exact capability name, not a name/summary
/// substring search, restricted via `--repo` to repos [`is_base_distro_repo`]
/// trusts.
///
/// This exists because the queue-filter checks elsewhere in this crate
/// (`try_list_project_packages` / `get_project_repo_paths`, matched by
/// [`fuzzy_key`]) only ever compare against *source package names* in OBS
/// projects — and a C library's RPM package name routinely embeds its
/// SONAME version (`libpng16`, `libopenssl3`) where the bare upstream
/// project name (`libpng`, `openssl`) does not. No separator/case
/// normalization bridges that gap, so a dependency the distro has shipped
/// for years still reads as "not found anywhere" and gets queued for a
/// redundant from-source build (observed: `libpng`, already `libpng16-16`
/// / `libpng16-devel` in openSUSE Tumbleweed, got probed, cloned from
/// upstream, and queued as a brand-new from-source C package).
///
/// `zypper search --provides` matches the actual RPM `Provides:` capability
/// list, not just the package's own name, so it catches exactly this case:
/// `libpng16-16` carries a compatibility `Provides: libpng` even though its
/// package name doesn't contain the string. `--match-exact` avoids the
/// opposite failure mode — a plain substring search would also match
/// unrelated packages that merely contain `capability` somewhere in a
/// longer provides string.
///
/// Restricting to [`is_base_distro_repo`] repos matters as much as
/// `--match-exact` does: an unrestricted search also matches whatever else
/// happens to be enabled locally (a personal `home:*` project, Packman, a
/// cloud vendor's own repo, …), none of which the actual OBS build target
/// resolves against — treating one of *those* as "the distro already has
/// it" would silently and permanently drop a dependency the build can never
/// actually satisfy, which is worse than today's "queue a redundant
/// from-source build". If no local repo is recognized as a base-distro repo
/// at all, this returns `false` rather than guessing from the full,
/// untrusted repo set.
///
/// This still assumes the local machine tracks the same distribution the
/// build target does (true for this tool's own use: it runs on the same
/// openSUSE Tumbleweed the target project builds against). A stale or
/// missing local repo cache only produces a false negative — capability
/// reported as not provided — which just falls through to the existing
/// from-source-build path, so it fails open to today's behavior rather than
/// silently skipping a dependency that's actually missing.
pub fn distro_provides(capability: &str) -> bool {
    let capability = capability.trim();
    // A name an agent reported is passed to zypper as an argument: one that
    // starts with `-` would be read as an option.
    if capability.is_empty() || capability.starts_with('-') {
        return false;
    }

    if let Some(hit) = DISTRO_PROVIDES_CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .and_then(|cache| cache.get(capability).copied())
    {
        return hit;
    }

    let trusted_aliases: Vec<String> = configured_repo_urls()
        .into_iter()
        .filter(|(_, url)| is_base_distro_repo(url))
        .map(|(alias, _)| alias)
        .collect();
    // `None` means the query itself failed (timeout, launch, ...): that is
    // not an answer, so it is neither cached nor allowed to stick for the run.
    let provided = if trusted_aliases.is_empty() {
        Some(false)
    } else {
        // `--repo` is a `search` subcommand option, not a global one — it
        // must come after `search`, not before it (zypper rejects it
        // outright, "The flag --repo is not known", otherwise).
        let mut args: Vec<String> = vec![
            "--non-interactive".into(),
            "--no-refresh".into(),
            "search".into(),
        ];
        for alias in &trusted_aliases {
            args.push("--repo".into());
            args.push(alias.clone());
        }
        args.push("--provides".into());
        args.push("--match-exact".into());
        args.push(capability.to_string());
        match proc::capture_with_timeout(
            "zypper",
            &args,
            &anywhere(),
            Emit::quiet(),
            ZYPPER_QUERY_TIMEOUT,
        ) {
            Ok(out) if out.success() => Some(true),
            // zypper's "capability not found" exit: a real answer, cacheable.
            Ok(out) if out.code == ZYPPER_CAPABILITY_NOT_FOUND => Some(false),
            Ok(out) => {
                ap_core::diag!(
                    "  zypper search for {capability}: exit {}: {}",
                    out.code,
                    first_line(&out.stderr)
                );
                None
            }
            Err(e) => {
                ap_core::diag!("  zypper search for {capability}: {e}");
                None
            }
        }
    };

    let Some(provided) = provided else {
        return false;
    };
    DISTRO_PROVIDES_CACHE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get_or_insert_with(HashMap::new)
        .insert(capability.to_string(), provided);
    provided
}

/// zypper's exit code for "capability not found" (`ZYPPER_EXIT_INF_CAP_NOT_FOUND`).
const ZYPPER_CAPABILITY_NOT_FOUND: i32 = 104;
