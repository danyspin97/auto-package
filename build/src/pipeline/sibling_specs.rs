//! The other specs of a package that has several (dbus-1 and dbus-1-devel-doc,
//! elfutils and elfutils-debuginfod). They share the upstream release, so a
//! version bump moves each one that is at the old version with the main spec. An
//! archive of the old version is removed only when no spec of the package names
//! it any more.
//!
//! The `.changes` entry of a sibling is not written here: the changes file of a
//! multi-spec package is chosen by the osc changes code, which this depends on.

use super::*;

/// The `*.spec` files of `checkout_dir` other than `spec_path`, as `(path, text)`,
/// in name order.
fn sibling_specs(checkout_dir: &Path, spec_path: &Path) -> Vec<(PathBuf, String)> {
    let mut paths: Vec<PathBuf> = std::fs::read_dir(checkout_dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "spec") && p != spec_path)
        .collect();
    paths.sort();
    paths
        .into_iter()
        .filter_map(|p| {
            let text = std::fs::read_to_string(&p).ok()?;
            Some((p, text))
        })
        .collect()
}

/// The sources the other specs of the package name, for the old-archive check.
pub(super) fn sibling_spec_sources(
    checkout_dir: &Path,
    spec_path: &Path,
) -> Vec<crate::spec::SourceEntry> {
    sibling_specs(checkout_dir, spec_path)
        .iter()
        .flat_map(|(_, text)| crate::spec::expanded_sources(text))
        .collect()
}

/// `text` with `old` replaced by `new` on its `%setup`/`%autosetup` lines, where a
/// literal version names the unpacked directory (`-n elfutils-0.196`).
fn rewrite_unpack_dirs(text: &str, old: &str, new: &str) -> String {
    text.split_inclusive('\n')
        .map(|line| {
            let head = line.trim_start();
            if (head.starts_with("%setup") || head.starts_with("%autosetup")) && line.contains(old)
            {
                line.replace(old, new)
            } else {
                line.to_string()
            }
        })
        .collect()
}

/// The other specs bumped with the main spec. Dropping it without [`keep`] puts
/// their original text back, so a failed update leaves no sibling at the new
/// version (the main spec stays as it was too).
///
/// [`keep`]: SiblingBump::keep
pub(super) struct SiblingBump {
    originals: Vec<(PathBuf, String)>,
    kept: bool,
}

impl SiblingBump {
    /// Set `Version:` to `new` in each other spec whose version is `old`, the
    /// version the main spec had before the bump, and rewrite a literal `old` in
    /// its unpack lines. A spec at another version keeps its own and is reported.
    pub(super) fn apply(
        checkout_dir: &Path,
        spec_path: &Path,
        old: &str,
        new: &str,
        rpm_name: &str,
        reporter: &dyn Reporter,
        step: ap_core::event::StepId,
    ) -> Self {
        let mut bump = SiblingBump {
            originals: Vec::new(),
            kept: false,
        };
        for (path, text) in sibling_specs(checkout_dir, spec_path) {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let Some(header) = crate::spec::parse_header(&text) else {
                continue;
            };
            if header.version != old {
                reporter.report(&Event::Warning {
                    step: Some(step),
                    message: format!(
                        "{rpm_name}: left the sibling spec {name} as it is: it is at {}, \
                         not at {old}, the version the main spec had",
                        header.version
                    ),
                });
                continue;
            }
            let Some(set) = crate::spec::set_version(&text, new) else {
                reporter.report(&Event::Warning {
                    step: Some(step),
                    message: format!(
                        "{rpm_name}: left the sibling spec {name}: its Version: is not a plain value"
                    ),
                });
                continue;
            };
            let bumped_text = rewrite_unpack_dirs(&set, old, new);
            match ap_core::fsx::write_atomic(&path, &bumped_text) {
                Ok(()) => {
                    ap_core::diag!("  {name}: Version set to {new} with the main spec");
                    bump.originals.push((path, text));
                }
                Err(e) => reporter.report(&Event::Warning {
                    step: Some(step),
                    message: format!("{rpm_name}: left the sibling spec {name}: {e}"),
                }),
            }
        }
        bump
    }

    /// The update went through: the bumped siblings stay.
    pub(super) fn keep(mut self) {
        self.kept = true;
    }
}

impl Drop for SiblingBump {
    fn drop(&mut self) {
        if !self.kept {
            for (path, text) in &self.originals {
                let _ = ap_core::fsx::write_atomic(path, text);
            }
        }
    }
}
