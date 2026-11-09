//! Sibling projects: their names, checkouts and RPMs, and the facts read from them.

use super::*;

/// The names a spec's `BuildRequires:`/`Requires:` lines depend on, lower-cased
/// (`%{python_module x}` and `python3dist(x)` give `x`).
fn spec_dependency_names(spec: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in spec.lines() {
        let line = line.trim();
        let Some((tag, value)) = line.split_once(':') else {
            continue;
        };
        if !(tag.eq_ignore_ascii_case("BuildRequires") || tag.eq_ignore_ascii_case("Requires")) {
            continue;
        }
        for token in value.split(|c: char| c == ',' || c.is_whitespace()) {
            let token = token
                .trim_matches(|c| c == '{' || c == '}' || c == '(' || c == ')')
                .trim_start_matches("python3dist(")
                .trim_end_matches(')');
            if token.is_empty()
                || token.starts_with(['%', '<', '>', '=', '/'])
                || token.contains(['(', '%'])
                || token.chars().next().is_some_and(|c| c.is_ascii_digit())
                || token == "python_module"
            {
                continue;
            }
            let token = token.to_lowercase();
            if !out.contains(&token) {
                out.push(token);
            }
        }
    }
    out
}

/// Sibling packages that have a checkout in the project, among a spec's
/// dependencies.
pub(super) fn sibling_names(spec: &str, available: &[String], own: &str) -> Vec<String> {
    let own_key = osc::project_key(own);
    let mut out: Vec<String> = Vec::new();
    for dep in spec_dependency_names(spec) {
        for cand in [dep.clone(), osc::rpm_name(&dep)] {
            if let Some(hit) = available
                .iter()
                .find(|a| osc::project_key(a) == osc::project_key(&cand))
                && osc::project_key(hit) != own_key
                && !out.contains(hit)
            {
                out.push(hit.clone());
            }
        }
    }
    out.truncate(12);
    out
}

/// Checkouts, in the project, of the packages `spec`'s package depends on.
pub(super) fn sibling_checkouts(config: &PipelineConfig, spec: &str) -> Vec<PathBuf> {
    let project_dir = config.work_dir.join(&config.project);
    let available: Vec<String> = std::fs::read_dir(&project_dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| {
            let n = e.file_name();
            e.path()
                .join(format!("{}.spec", n.to_string_lossy()))
                .is_file()
        })
        .filter_map(|e| e.file_name().into_string().ok())
        .collect();
    sibling_names(spec, &available, &config.rpm_name)
        .into_iter()
        .map(|n| project_dir.join(n))
        .collect()
}

/// Whether `rest`, the part of a file name after `<name>-`, is
/// `<version>-<release>.<arch>.rpm`. Exactly two `-` fields: a version that
/// starts with a digit, then a release with the arch after its last dot. A
/// sibling package whose name extends `name` (`ffmpeg-8-libs-8.0-1.x86_64.rpm`
/// for `ffmpeg`) has more fields and does not match.
pub(super) fn is_version_release_arch_rpm(rest: &str) -> bool {
    let Some(stem) = rest.strip_suffix(".rpm") else {
        return false;
    };
    match stem.split('-').collect::<Vec<_>>().as_slice() {
        [version, release_arch] => {
            version.starts_with(|c: char| c.is_ascii_digit())
                && release_arch
                    .rsplit_once('.')
                    .is_some_and(|(release, arch)| !release.is_empty() && !arch.is_empty())
        }
        _ => false,
    }
}

/// The `.rpm` files of a sibling the project built, found under `rpms_dir`.
fn sibling_rpms(rpms_dir: &Path, name: &str) -> Vec<PathBuf> {
    fn walk(dir: &Path, name: &str, depth: usize, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let path = entry.path();
            let file = entry.file_name().to_string_lossy().into_owned();
            if path.is_dir() {
                if depth < 3 {
                    walk(&path, name, depth + 1, out);
                }
            } else if let Some(rest) = file.strip_prefix(&format!("{name}-"))
                && is_version_release_arch_rpm(rest)
                && !file.ends_with(".src.rpm")
            {
                out.push(path);
            }
        }
    }
    let mut out = Vec::new();
    walk(rpms_dir, name, 0, &mut out);
    out.sort();
    out.truncate(3);
    out
}

/// What the packages this one depends on look like, read from their checkouts and
/// built RPMs: the `%files` of each spec and each RPM's file list, so a turn can
/// tell what a sibling ships without needing the access to read it. Capped.
pub(super) fn sibling_facts(config: &PipelineConfig, dirs: &[PathBuf]) -> Option<String> {
    const TOTAL_CAP: usize = 24 * 1024;
    let mut text = String::new();
    for dir in dirs.iter().take(6) {
        let Some(name) = dir.file_name().map(|n| n.to_string_lossy().into_owned()) else {
            continue;
        };
        let spec_path = dir.join(format!("{name}.spec"));
        let spec = std::fs::read_to_string(&spec_path).unwrap_or_default();
        text.push_str(&format!(
            "### {name}\nspec (read-only): {}\n",
            spec_path.display()
        ));
        let files: Vec<&str> = spec
            .lines()
            .skip_while(|l| !l.starts_with("%files"))
            .take(40)
            .collect();
        if !files.is_empty() {
            text.push_str("its `%files`:\n");
            for l in files {
                text.push_str(&format!("  {l}\n"));
            }
        }
        for rpm in sibling_rpms(&config.rpms_dir, &name) {
            let listing = std::process::Command::new("rpm")
                .arg("-qlp")
                .arg(&rpm)
                .stdin(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .output()
                .ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
                .unwrap_or_default();
            let lines: Vec<&str> = listing.lines().take(60).collect();
            if lines.is_empty() {
                continue;
            }
            text.push_str(&format!(
                "files of {} (first {}):\n",
                rpm.file_name().unwrap_or_default().to_string_lossy(),
                lines.len()
            ));
            for l in lines {
                text.push_str(&format!("  {l}\n"));
            }
        }
        text.push('\n');
        if text.len() > TOTAL_CAP {
            // Cut back to a character boundary: the spec and RPM listings can
            // hold multi-byte text, and a mid-character cut would panic.
            let mut cut = TOTAL_CAP;
            while !text.is_char_boundary(cut) {
                cut -= 1;
            }
            text.truncate(cut);
            text.push_str("\n(truncated)\n");
            break;
        }
    }
    (!text.is_empty()).then(|| {
        format!(
            "Packages this one depends on (read-only; a change they need is requested with \
             `patch_dependencies`, not made by you):\n\n{text}"
        )
    })
}
