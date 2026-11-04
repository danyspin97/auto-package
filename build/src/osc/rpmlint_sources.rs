//! The rpmlint filter files of a checkout. A filter the pipeline wrote is
//! published only when a `Source:` line of the spec names it, so one that no
//! line names is given one here, and a changelog that says a filter was added
//! is checked against what is published.

use super::*;
use std::path::PathBuf;

/// Whether `name` is an rpmlint filter file (`<name>-rpmlintrc` or
/// `<name>.rpmlintrc`), and the package prefix it carries.
fn rpmlintrc_prefix(name: &str) -> Option<&str> {
    name.strip_suffix("-rpmlintrc")
        .or_else(|| name.strip_suffix(".rpmlintrc"))
}

/// The file in a run's workspace that lists the filter files this run wrote or
/// accepted, one name a line.
const WRITTEN: &str = "rpmlintrc-written";

/// Record that this run wrote or accepted the filter file `name`.
pub fn note_rpmlintrc_written(run_dir: &Path, name: &str) {
    use std::io::Write;
    if let Ok(mut list) = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(run_dir.join(WRITTEN))
    {
        let _ = writeln!(list, "{name}");
    }
}

/// The filter files this run wrote or accepted (see [`note_rpmlintrc_written`]).
pub fn rpmlintrcs_written(run_dir: &Path) -> Vec<String> {
    fs::read_to_string(run_dir.join(WRITTEN))
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

/// The filter files of `checkout_dir` that are not empty and that no spec of
/// the checkout names with a `Source:` line. The publish plan leaves these out
/// unless [`name_unnamed_rpmlintrcs`] names them.
pub fn unnamed_rpmlintrcs(checkout_dir: &Path) -> Vec<String> {
    let entries: Vec<(String, u64)> = fs::read_dir(checkout_dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let meta = e.metadata().ok()?;
            meta.is_file().then_some((name, meta.len()))
        })
        .collect();
    let spec_texts: Vec<String> = entries
        .iter()
        .filter(|(n, _)| n.ends_with(".spec"))
        .filter_map(|(n, _)| fs::read_to_string(checkout_dir.join(n)).ok())
        .collect();
    let sources = all_spec_source_names(&spec_texts);
    let mut names: Vec<String> = entries
        .into_iter()
        .filter(|(n, len)| *len > 0 && rpmlintrc_prefix(n).is_some())
        .map(|(n, _)| n)
        .filter(|n| !sources.iter().any(|p| matches_source_name(p, n)))
        .collect();
    names.sort();
    names
}

/// `spec` with a `Source<N>: name` line added after its last `Source` line, N
/// one above the highest there. `None` when the spec has no `Source` line.
pub fn with_rpmlintrc_source(spec: &str, name: &str) -> Option<String> {
    let lines: Vec<&str> = spec.split_inclusive('\n').collect();
    let (mut last, mut highest) = (None, None::<u32>);
    for (i, line) in lines.iter().enumerate() {
        if let Some(source) = crate::spec::parse_source_line(line, &["source"], str::to_string) {
            last = Some(i);
            highest = Some(highest.map_or(source.index, |h| h.max(source.index)));
        }
    }
    let (last, highest) = (last?, highest?);
    let mut out = String::with_capacity(spec.len() + name.len() + 32);
    for line in &lines[..=last] {
        out.push_str(line);
    }
    if !lines[last].ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&format!("Source{}:        {name}\n", highest + 1));
    for line in &lines[last + 1..] {
        out.push_str(line);
    }
    Some(out)
}

/// Names the filter files this run wrote or accepted, which no spec names, in
/// the spec that builds them: `<prefix>.spec` for `<prefix>-rpmlintrc`. All the
/// edits are made in memory first, and the specs are written only when every
/// name found its spec, so a refusal leaves the checkout as it was. Returns the
/// names added and the unnamed files this run did not write (left out).
pub fn name_unnamed_rpmlintrcs(
    checkout_dir: &Path,
    written: &[String],
) -> Result<(Vec<String>, Vec<String>), String> {
    let mut added = Vec::new();
    let mut left_out = Vec::new();
    let mut edits: std::collections::BTreeMap<PathBuf, String> = Default::default();
    for name in unnamed_rpmlintrcs(checkout_dir) {
        if !written.contains(&name) {
            left_out.push(name);
            continue;
        }
        let prefix = rpmlintrc_prefix(&name).unwrap_or_default().to_string();
        let spec = checkout_dir.join(format!("{prefix}.spec"));
        let text = match edits.get(&spec) {
            Some(text) => text.clone(),
            None => fs::read_to_string(&spec).unwrap_or_default(),
        };
        match with_rpmlintrc_source(&text, &name) {
            Some(new) => {
                edits.insert(spec, new);
                added.push(name);
            }
            None => {
                return Err(format!(
                    "rpmlintrc {name} exists but no Source names it, and {} has no `Source:` \
                     line to follow: add a line `Source<N>: {name}` to the spec that builds it \
                     (or drop the file); nothing was sent to OBS",
                    spec.file_name().unwrap_or_default().to_string_lossy()
                ));
            }
        }
    }
    for (spec, text) in edits {
        ap_core::fsx::write_atomic(&spec, text)
            .map_err(|e| format!("writing {}: {e}", spec.display()))?;
    }
    Ok((added, left_out))
}

/// The top entry of the newest changelog, as its lines.
fn newest_entry(checkout_dir: &Path) -> Vec<String> {
    let Some(file) = newest_changes_file(checkout_dir) else {
        return Vec::new();
    };
    let text = fs::read_to_string(file).unwrap_or_default();
    let mut seps = 0;
    let mut entry = Vec::new();
    for line in text.lines() {
        if line.starts_with("-------") {
            seps += 1;
            if seps == 2 {
                break;
            }
            continue;
        }
        if seps >= 1 {
            entry.push(line.to_string());
        }
    }
    entry
}

/// Whether a changelog line says an rpmlint filter was added: it names rpmlint
/// and a filter, starts with an adding verb, and says nothing of removing.
fn claims_a_filter(line: &str) -> bool {
    const ADDS: &[&str] = &[
        "add",
        "added",
        "adds",
        "adding",
        "filter",
        "filtered",
        "filters",
        "filtering",
        "ignore",
        "ignored",
        "silence",
        "silenced",
        "suppress",
        "suppressed",
    ];
    const REMOVES: &[&str] = &[
        "remove",
        "removed",
        "drop",
        "dropped",
        "delete",
        "deleted",
        "no longer",
    ];
    let l = line
        .trim()
        .trim_start_matches('-')
        .trim()
        .to_ascii_lowercase();
    let first = l.split_whitespace().next().unwrap_or("");
    l.contains("rpmlint") && ADDS.contains(&first) && !REMOVES.iter().any(|r| l.contains(r))
}

/// The reason the newest changelog entry claims an rpmlint filter that the
/// published files do not carry, or `None`. `published` are the names the
/// commit would send.
pub fn filter_claim_without_rpmlintrc(checkout_dir: &Path, published: &[String]) -> Option<String> {
    let claims = newest_entry(checkout_dir)
        .iter()
        .any(|l| claims_a_filter(l));
    let carried = published.iter().any(|n| rpmlintrc_prefix(n).is_some());
    (claims && !carried).then(|| {
        "the changelog says an rpmlint filter was added, but no rpmlintrc is published; \
         nothing was sent to OBS"
            .to_string()
    })
}

#[cfg(test)]
mod claim_tests {
    use super::claims_a_filter;

    #[test]
    fn an_added_filter_is_claimed() {
        assert!(claims_a_filter(
            "- Filter the no-binary rpmlint error for the UEFI applications"
        ));
        assert!(claims_a_filter(
            "- Add an rpmlint filter for the static library"
        ));
    }

    #[test]
    fn a_removed_filter_is_not_claimed() {
        assert!(!claims_a_filter(
            "- Remove the rpmlint filter, no longer needed"
        ));
        assert!(!claims_a_filter(
            "- Drop the rpmlintrc, rpmlint is clean now"
        ));
    }

    #[test]
    fn a_line_without_rpmlint_or_an_adding_verb_is_not_claimed() {
        assert!(!claims_a_filter("- Bump to 1.9"));
        assert!(!claims_a_filter("- Add a patch for the build"));
    }
}
