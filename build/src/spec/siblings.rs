//! Sibling wheels of a monorepo: finding them, their packages in sitelib, and the
//! relocation of their files.

use super::*;

/// The names of the `siblings` that own (hold the `__init__.py` of) a package
/// directory `layout` adds files to: this wheel completes their package, so it
/// needs them installed (convention 76 (section 83) a). A directory this wheel owns itself is
/// not listed. Sorted, without duplicates.
pub(super) fn shared_package_owners(
    layout: &ap_ecosystem::sdist::WheelLayout,
    siblings: &[SiblingWheel],
) -> Vec<String> {
    let mut owners: Vec<String> = Vec::new();
    for SiblingWheel { name, files, .. } in siblings {
        let owns = files.iter().any(|f| {
            let Some(dir) = f.strip_suffix("/__init__.py") else {
                return false;
            };
            let prefix = format!("{dir}/");
            let own_init = layout.files.iter().any(|m| m == f);
            !own_init && layout.files.iter().any(|m| m.starts_with(&prefix))
        });
        if owns && !owners.contains(name) {
            owners.push(name.clone());
        }
    }
    owners.sort();
    owners
}

/// Another wheel of the same project (convention 76 (section 83)): `name` is the
/// flavor-neutral RPM name, `files` what it installs relative to site-packages
/// (its dist-info left out), `purelib` its `Root-Is-Purelib` when known.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SiblingWheel {
    pub name: String,
    pub files: Vec<String>,
    pub purelib: Option<bool>,
}

/// Whether a sibling that declares `Root-Is-Purelib: true` fills a top-level
/// directory of `layout`: the whole tree then lives in `%{python_sitelib}`.
pub(super) fn sibling_forces_purelib(
    layout: &ap_ecosystem::sdist::WheelLayout,
    siblings: &[SiblingWheel],
) -> bool {
    let tops = payload_tops(layout);
    siblings.iter().any(|s| {
        s.purelib == Some(true)
            && s.files
                .iter()
                .any(|f| f.split_once('/').is_some_and(|(t, _)| tops.contains(&t)))
    })
}

/// The `%install` lines that move a wheel installed into `%{python_sitearch}`
/// (it says `Root-Is-Purelib: false`) into `%{python_sitelib}`, beside the
/// sibling wheels that fill the same tree.
pub(super) fn sitelib_relocation(layout: &ap_ecosystem::sdist::WheelLayout) -> String {
    let mut text = String::from(
        "# the wheel says Root-Is-Purelib: false, but a sibling wheel of this project (a\n\
         # noarch package) fills the same tree in sitelib, and a package tree split\n\
         # between lib and lib64 cannot be imported: install it beside them (convention 76 (section 83))\n\
         %python_expand mkdir -p %{buildroot}%{$python_sitelib}\n",
    );
    let mut tops: Vec<&str> = payload_tops(layout);
    if let Some(d) = layout.dist_info.as_deref()
        && ap_ecosystem::sdist::is_shell_safe_member(d)
    {
        tops.push(d);
    }
    for top in tops {
        text.push_str(&format!(
            "%python_expand mv %{{buildroot}}%{{$python_sitearch}}/{top} %{{buildroot}}%{{$python_sitelib}}/\n"
        ));
    }
    text
}

/// `spec`, generated for a wheel before a sibling wheel that fills the same
/// tree in `%{python_sitelib}` was known (convention 76 (section 83)), moved onto that one
/// tree: python-tokenspeed-mla failed with `No module named 'cutlass.cute'`
/// because `-base` and `-cu12` had been generated into sitearch while `-core`
/// is noarch. Every `sitearch` of the spec becomes `sitelib` and the wheel's
/// tree is moved there right after the install ([`sitelib_relocation`]), as the
/// generator does when it knows the siblings. A spec that does not use
/// sitearch, or already relocates, is returned as is.
pub fn with_sibling_tree_in_sitelib(
    spec: &str,
    layout: &ap_ecosystem::sdist::WheelLayout,
    siblings: &[SiblingWheel],
) -> String {
    if !sibling_forces_purelib(layout, siblings)
        || !spec.contains("python_sitearch")
        || spec.contains("mv %{buildroot}%{$python_sitearch}/")
    {
        return spec.to_string();
    }
    let tail = if spec.ends_with('\n') { "\n" } else { "" };
    let mut out: Vec<String> = Vec::new();
    for line in spec.lines() {
        out.push(
            line.replace("%{$python_sitearch}", "%{$python_sitelib}")
                .replace("%{python_sitearch}", "%{python_sitelib}"),
        );
    }
    // After the last install line.
    let at = out.iter().rposition(|l| {
        let t = l.trim_start();
        t.starts_with("%pyproject_install")
            || (t.starts_with("%python") && t.contains("_pyproject_install"))
    });
    if let Some(at) = at {
        let reloc: Vec<String> = sitelib_relocation(layout)
            .lines()
            .map(str::to_string)
            .collect();
        for (k, l) in reloc.into_iter().enumerate() {
            out.insert(at + 1 + k, l);
        }
    } else {
        return spec.to_string();
    }
    out.join("\n") + tail
}

/// The other wheels of `pkg_name`'s project, from every source that knows what
/// they install, in a fixed order so the result does not depend on which was
/// built first:
///  1. the wheel file in the sibling's own checkout (`<checkouts>/python-<name>/
///     *.whl`, the first by name), listed from the zip: present as soon as that
///     package's spec was generated, built or not;
///  2. otherwise the RPM already built in `rpms_dir`, listed with `rpm -qlp`.
///
/// A sibling is a package whose name shares all but the last two dash-separated
/// words (at least two) with `pkg_name`: `nvidia-cutlass-dsl-libs-base`/`-core`/
/// `-cu13`. Without any source the result is empty.
pub fn sibling_wheels(
    rpms_dir: &std::path::Path,
    checkouts: Option<&std::path::Path>,
    pkg_name: &str,
) -> Vec<SiblingWheel> {
    let mut out: Vec<SiblingWheel> = checkouts
        .map(|c| sibling_wheels_in_checkouts(c, pkg_name))
        .unwrap_or_default();
    for (name, files, purelib) in sibling_rpm_files(rpms_dir, pkg_name) {
        if !out.iter().any(|s| s.name == name) {
            out.push(SiblingWheel {
                name,
                files,
                purelib,
            });
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// The siblings among the `python-*` package checkouts in `checkouts` that hold
/// a wheel (see [`sibling_wheels`]).
fn sibling_wheels_in_checkouts(checkouts: &std::path::Path, pkg_name: &str) -> Vec<SiblingWheel> {
    let Ok(entries) = std::fs::read_dir(checkouts) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for e in entries.flatten() {
        let dir = e.file_name().to_string_lossy().into_owned();
        let Some(name) = dir.strip_prefix("python-") else {
            continue;
        };
        let name = name.to_lowercase();
        if !is_sibling_project(&name, pkg_name) {
            continue;
        }
        let Ok(inner) = std::fs::read_dir(e.path()) else {
            continue;
        };
        let mut wheels: Vec<std::path::PathBuf> = inner
            .flatten()
            .map(|f| f.path())
            .filter(|p| p.extension().is_some_and(|x| x == "whl") && p.is_file())
            .collect();
        wheels.sort();
        let Some(wheel) = wheels.first() else {
            continue;
        };
        // Names only: reading a full layout would open the libraries of an
        // NVIDIA wheel, and this runs for every sibling.
        let Some(names) = ap_ecosystem::sdist::wheel_file_names(wheel) else {
            continue;
        };
        let files: Vec<String> = names
            .into_iter()
            .filter(|f| {
                f.split_once('/')
                    .is_some_and(|(top, _)| !top.ends_with(".dist-info"))
            })
            .collect();
        if files.is_empty() {
            continue;
        }
        out.push(SiblingWheel {
            name: format!("python-{name}"),
            files,
            purelib: ap_ecosystem::sdist::wheel_root_is_purelib(wheel),
        });
    }
    out
}

/// The RPMs in `rpms_dir` built for the other wheels of `pkg_name`'s project,
/// as `(flavor-neutral name, files relative to site-packages, purelib)`, one
/// flavor per package. Asking `rpm` for the file lists is the only way to know
/// what a sibling really installs once built; without `rpm` the result is empty.
fn sibling_rpm_files(
    rpms_dir: &std::path::Path,
    pkg_name: &str,
) -> Vec<(String, Vec<String>, Option<bool>)> {
    let Ok(entries) = std::fs::read_dir(rpms_dir) else {
        return Vec::new();
    };
    let mut paths: Vec<(String, std::path::PathBuf)> = entries
        .flatten()
        .filter_map(|e| {
            let file = e.file_name().to_string_lossy().into_owned();
            sibling_rpm_name(&file, pkg_name).map(|_| (file, e.path()))
        })
        .collect();
    paths.sort();
    let mut out: Vec<(String, Vec<String>, Option<bool>)> = Vec::new();
    for (file, path) in paths {
        let Some(name) = sibling_rpm_name(&file, pkg_name) else {
            continue;
        };
        if out.iter().any(|(n, _, _)| *n == name) {
            continue;
        }
        let Ok(listing) = std::process::Command::new("rpm")
            .args(["-qlp", "--nosignature"])
            .arg(&path)
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
        else {
            return Vec::new();
        };
        if !listing.status.success() {
            continue;
        }
        let text = String::from_utf8_lossy(&listing.stdout);
        let files: Vec<String> = site_package_files(&text)
            .into_iter()
            .filter(|f| {
                f.split('/')
                    .next()
                    .is_some_and(|t| !t.ends_with(".dist-info"))
                    && f.contains('/')
            })
            .collect();
        if !files.is_empty() {
            let purelib = if text.contains("/usr/lib64/") {
                Some(false)
            } else if text.contains("/usr/lib/python") {
                Some(true)
            } else {
                None
            };
            out.push((name, files, purelib));
        }
    }
    out
}

/// Whether the (lowercase, dash-separated) package `name` is another wheel of
/// the project `pkg_name` belongs to: not `pkg_name` itself, and sharing all
/// but the last two words with it, at least two.
fn is_sibling_project(name: &str, pkg_name: &str) -> bool {
    let own = pkg_name.to_lowercase().replace('_', "-");
    if name == own || name.ends_with("-debuginfo") || name.ends_with("-debugsource") {
        return false;
    }
    let a: Vec<&str> = name.split('-').collect();
    let b: Vec<&str> = own.split('-').collect();
    let common = a.iter().zip(&b).take_while(|(x, y)| x == y).count();
    let need = a.len().min(b.len()).saturating_sub(2).max(2);
    common >= need
}

/// The flavor-neutral package name of the RPM file `file` when it belongs to a
/// sibling wheel project of `pkg_name` (see [`sibling_wheels`]); `None`
/// for another project, for `pkg_name` itself, and for a source or debug RPM.
pub(super) fn sibling_rpm_name(file: &str, pkg_name: &str) -> Option<String> {
    let stem = file.strip_suffix(".rpm")?;
    if stem.ends_with(".src") || stem.ends_with(".nosrc") {
        return None;
    }
    let rest = stem.strip_prefix("python3")?;
    let digits = rest.chars().take_while(|c| c.is_ascii_digit()).count();
    if digits == 0 {
        return None;
    }
    let base = rest[digits..].strip_prefix('-')?;
    // `<name>-<version>-<release>.<arch>`: cut the last two dashes.
    let mut parts = base.rsplitn(3, '-');
    let (_release, _version, name) = (parts.next()?, parts.next()?, parts.next()?);
    let name = name.to_lowercase();
    is_sibling_project(&name, pkg_name).then(|| format!("python-{name}"))
}

/// The paths below `site-packages` in an `rpm -ql` listing, for files only
/// (directory lines end the same way, so a path that is a prefix of another is a
/// directory and dropped).
pub(super) fn site_package_files(listing: &str) -> Vec<String> {
    let mut paths: Vec<String> = listing
        .lines()
        .filter_map(|l| {
            l.split_once("/site-packages/")
                .map(|(_, p)| p.trim().to_string())
        })
        .filter(|p| !p.is_empty())
        .collect();
    paths.sort();
    paths.dedup();
    let all = paths.clone();
    paths.retain(|p| !all.iter().any(|q| q.starts_with(&format!("{p}/"))));
    paths
}
