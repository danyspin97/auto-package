//! The patch headers of `lint`: a committed patch whose headers name quilt's
//! `source/` and `source.orig/` directories, where the archive's top directory
//! is meant. The audit reports each such patch, and tidy writes the headers
//! back (the pipeline's refresh does the same after a quilt turn, which is the
//! only other place these prefixes are rewritten). A carrying package's patch
//! that its origin has the same problem in is inherited, as its other findings.

use super::leftovers::Carry;
use super::*;
use ap_build::patch_check::rewrite_header_prefixes;
use ap_build::quality::{Finding, Kind};

/// The archive's top directory of `spec` for `pkg`: the prefix its patch
/// headers name. `None` when the spec has no version or no usable directory.
fn top_dir(pkg: &str, spec: &str) -> Option<String> {
    let version = ap_build::spec::parse_header(spec)?.version;
    ap_build::spec::autosetup_expected_dir(spec, pkg, &version)
}

/// `text` with its header prefixes made `top` (see `rewrite_header_prefixes`),
/// `None` when nothing changes.
fn rewritten(text: &str, top: &str) -> Option<String> {
    rewrite_header_prefixes(text.as_bytes(), top).and_then(|b| String::from_utf8(b).ok())
}

/// The names of the `patches` (name, text) of `spec` whose headers would be
/// rewritten. Empty when the spec names no top directory.
pub(super) fn due(pkg: &str, spec: &str, patches: &[(String, String)]) -> Vec<String> {
    let Some(top) = top_dir(pkg, spec) else {
        return Vec::new();
    };
    patches
        .iter()
        .filter(|(_, text)| rewritten(text, &top).is_some())
        .map(|(name, _)| name.clone())
        .collect()
}

/// The header findings of a package and the patches the tidy rewrites for them.
#[derive(Debug, Default)]
pub(super) struct HeaderAudit {
    /// The findings the package has of its own, or all of them with `--fix`.
    pub(super) own: Vec<Finding>,
    /// The findings its origin has too: listed, fixed only with `--fix`.
    pub(super) inherited: Vec<Finding>,
    /// The patches the tidy rewrites: the patches of `own`.
    pub(super) scope: Vec<String>,
}

fn finding(name: &str) -> Finding {
    Finding {
        kind: Kind::PatchHeaders,
        detail: format!("{name}: headers name quilt's source/ directory"),
    }
}

/// The header audit of `pkg`'s `patches` with its `spec` (pure: no reads).
/// `origin` is the origin's package, spec and patches when the package carries
/// work: a patch whose origin copy has the same problem is inherited, unless
/// `fix_inherited`.
pub(super) fn split(
    pkg: &str,
    spec: &str,
    patches: &[(String, String)],
    origin: Option<(&str, &str, &[(String, String)])>,
    fix_inherited: bool,
) -> HeaderAudit {
    let origin_due = origin
        .map(|(origin_pkg, origin_spec, origin_patches)| {
            due(origin_pkg, origin_spec, origin_patches)
        })
        .unwrap_or_default();
    let mut out = HeaderAudit::default();
    for name in due(pkg, spec, patches) {
        if origin_due.contains(&name) && !fix_inherited {
            out.inherited.push(finding(&name));
        } else {
            out.own.push(finding(&name));
            out.scope.push(name);
        }
    }
    out
}

/// The header audit of a package the audit reads. Not carried: its `spec` and
/// `source_patches` (read from `source`) are the package's. Carried: the work's
/// spec and patches are read from `carry.work`, the origin's from `carry.origin`
/// (the source's own copy is reused when they are the same). A read that fails
/// leaves the package unsplit, every header finding own, as the other findings
/// are when the work cannot be compared.
#[allow(clippy::too_many_arguments)]
pub(super) fn audit(
    carry: Option<&Carry>,
    source: &str,
    pkg: &str,
    spec: &str,
    source_patches: &[(String, String)],
    fix_inherited: bool,
    emit: Emit<'_>,
    warn: &dyn Fn(String),
) -> HeaderAudit {
    let Some(carry) = carry else {
        return split(pkg, spec, source_patches, None, fix_inherited);
    };
    let work = match update::spec_for_package(&carry.work, pkg, emit) {
        Ok(work) => work,
        Err(e) => {
            warn(format!(
                "{pkg}: its branch work was not read for its patch headers, so all of them are checked: {e}"
            ));
            return split(pkg, spec, source_patches, None, fix_inherited);
        }
    };
    let work_patches = super::provenance::read_patches(&carry.work, pkg, &work, emit, warn);
    let origin = if carry.origin == source && carry.origin_pkg == pkg {
        Ok((spec.to_string(), source_patches.to_vec()))
    } else {
        update::spec_for_package(&carry.origin, &carry.origin_pkg, emit)
            .map_err(|e| e.to_string())
            .map(|origin_spec| {
                let patches = super::provenance::read_patches(
                    &carry.origin,
                    &carry.origin_pkg,
                    &origin_spec,
                    emit,
                    warn,
                );
                (origin_spec, patches)
            })
    };
    match origin {
        Ok((origin_spec, origin_patches)) => split(
            pkg,
            &work,
            &work_patches,
            Some((&carry.origin_pkg, &origin_spec, &origin_patches)),
            fix_inherited,
        ),
        Err(e) => {
            warn(format!(
                "{pkg}: its origin's patch headers were not read, so all of them are checked: {e}"
            ));
            split(pkg, &work, &work_patches, None, fix_inherited)
        }
    }
}

/// The patches of `patches` the tidy rewrites: those named in `scope`.
pub(super) fn scoped(patches: Vec<(String, String)>, scope: &[String]) -> Vec<(String, String)> {
    patches
        .into_iter()
        .filter(|(name, _)| scope.contains(name))
        .collect()
}

/// `edits` with the header prefixes of `originals` (the checkout's patches as
/// they are on disk) made the top directory of `spec`. A patch the edits already
/// change is rewritten from its new text, so its `before` stays the disk text
/// that a failed commit restores.
pub(super) fn with_edits(
    pkg: &str,
    spec: &str,
    originals: &[(String, String)],
    mut edits: super::provenance::ProvenanceEdits,
) -> super::provenance::ProvenanceEdits {
    let Some(top) = top_dir(pkg, spec) else {
        return edits;
    };
    for (name, before) in originals {
        match edits.patches.iter().position(|(n, _, _)| n == name) {
            Some(i) => {
                if let Some(text) = rewritten(&edits.patches[i].2, &top) {
                    edits.patches[i].2 = text;
                }
            }
            None => {
                if let Some(text) = rewritten(before, &top) {
                    edits.patches.push((name.clone(), before.clone(), text));
                }
            }
        }
    }
    edits
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPEC: &str = "Name: python-Genshi\nVersion: 0.7.9\nSource: Genshi-%{version}.tar.gz\n\n%prep\n%autosetup -p1 -n Genshi-%{version}\n\n%check\n%pytest\n";

    const SOURCE_HEADERS: &str = "Index: source/genshi/core.py\n===================================================================\n--- source.orig/genshi/core.py\n+++ source/genshi/core.py\t2024-01-01\n@@ -1 +1 @@\n-a\n+b\n";

    const AB_HEADERS: &str = "--- a/genshi/core.py\n+++ b/genshi/core.py\n@@ -1 +1 @@\n-a\n+b\n";

    const TOP_HEADERS: &str = "--- Genshi-0.7.9.orig/genshi/core.py\n+++ Genshi-0.7.9/genshi/core.py\n@@ -1 +1 @@\n-a\n+b\n";

    fn patch(name: &str, text: &str) -> (String, String) {
        (name.to_string(), text.to_string())
    }

    #[test]
    fn a_patch_with_quilt_source_headers_is_due() {
        let patches = [patch("py315-fixes.patch", SOURCE_HEADERS)];
        assert_eq!(due("python-Genshi", SPEC, &patches), ["py315-fixes.patch"]);
    }

    #[test]
    fn a_patch_with_a_b_headers_or_the_top_directory_is_not_due() {
        let patches = [
            patch("ab.patch", AB_HEADERS),
            patch("top.patch", TOP_HEADERS),
        ];
        assert!(due("python-Genshi", SPEC, &patches).is_empty());
    }

    #[test]
    fn a_spec_without_a_version_names_no_top_directory() {
        let patches = [patch("py315-fixes.patch", SOURCE_HEADERS)];
        assert!(due("python-Genshi", "Name: x\n", &patches).is_empty());
    }

    #[test]
    fn the_rewrite_is_idempotent_and_a_second_pass_finds_nothing() {
        let once = rewritten(SOURCE_HEADERS, "Genshi-0.7.9").expect("rewritten");
        assert!(once.contains("--- Genshi-0.7.9.orig/genshi/core.py\n"));
        assert!(once.contains("+++ Genshi-0.7.9/genshi/core.py\t2024-01-01\n"));
        assert!(due("python-Genshi", SPEC, &[patch("p.patch", &once)]).is_empty());
        assert_eq!(rewritten(&once, "Genshi-0.7.9"), None);
    }

    #[test]
    fn a_patch_the_edits_already_change_keeps_its_disk_text_as_before() {
        let originals = [patch("py315-fixes.patch", SOURCE_HEADERS)];
        let edits = super::super::provenance::ProvenanceEdits {
            spec: SPEC.to_string(),
            patches: vec![(
                "py315-fixes.patch".to_string(),
                SOURCE_HEADERS.to_string(),
                format!("Upstream: submitted\n{SOURCE_HEADERS}"),
            )],
        };
        let out = with_edits("python-Genshi", SPEC, &originals, edits);
        assert_eq!(out.patches.len(), 1);
        assert_eq!(out.patches[0].1, SOURCE_HEADERS);
        assert!(out.patches[0].2.starts_with("Upstream: submitted\n"));
        assert!(out.patches[0].2.contains("+++ Genshi-0.7.9/genshi/core.py"));
        assert!(!out.patches[0].2.contains("source/genshi"));
    }

    #[test]
    fn an_origin_with_the_same_header_problem_is_inherited_until_fix() {
        let branch = [
            patch("shared.patch", SOURCE_HEADERS),
            patch("branch.patch", SOURCE_HEADERS),
        ];
        let origin = [patch("shared.patch", SOURCE_HEADERS)];
        let fake = Some(("python-Genshi", SPEC, &origin[..]));
        let plain = split("python-Genshi", SPEC, &branch, fake, false);
        assert_eq!(plain.own.len(), 1, "the branch's own patch");
        assert_eq!(
            plain.scope,
            ["branch.patch"],
            "only own patches are rewritten"
        );
        assert_eq!(plain.inherited.len(), 1);
        assert!(plain.inherited[0].detail.starts_with("shared.patch: "));
        assert!(
            plain.inherited.iter().any(|f| f.kind.is_mechanical()),
            "an inherited header finding blocks the tidy, as a style one does"
        );
        let all = split("python-Genshi", SPEC, &branch, fake, true);
        assert!(all.inherited.is_empty());
        assert_eq!(all.scope, ["shared.patch", "branch.patch"]);
    }

    #[test]
    fn an_origin_that_is_fine_leaves_the_branch_patch_to_the_default_fix() {
        let branch = [patch("p.patch", SOURCE_HEADERS)];
        let origin = [patch("p.patch", AB_HEADERS)];
        let found = split(
            "python-Genshi",
            SPEC,
            &branch,
            Some(("python-Genshi", SPEC, &origin[..])),
            false,
        );
        assert!(found.inherited.is_empty());
        assert_eq!(found.scope, ["p.patch"]);
    }

    #[test]
    fn a_branch_patch_already_fixed_has_no_finding() {
        let branch = [patch("top.patch", TOP_HEADERS)];
        let found = split("python-Genshi", SPEC, &branch, None, false);
        assert!(found.own.is_empty() && found.scope.is_empty());
    }

    #[test]
    fn the_tidy_rewrites_only_the_patches_in_scope() {
        let patches = vec![
            patch("own.patch", SOURCE_HEADERS),
            patch("shared.patch", SOURCE_HEADERS),
        ];
        let kept = scoped(patches, &["own.patch".to_string()]);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].0, "own.patch");
    }
}
