//! The `_service` an agent edit leaves in the checkout. osc reads it at commit,
//! and a malformed one stops the commit after the build has run. An edit that
//! leaves one unreadable is rolled back with the rest of the edit, the way a
//! rejected spec is, and the agent is told why.

use super::*;
use crate::pipeline::render::*;

/// The problem with the `_service` an edit leaves, given what the snapshot held
/// before the turn: `None` for a file the turn did not change, or one that osc
/// reads. `before` is `None` when the file was absent, `Some(None)` when it was
/// there but its bytes were not kept (nothing to compare with). The file must be
/// UTF-8 (the parser that runs the services reads UTF-8 only), be XML with one
/// `<services>` root, and name only the services and params the allowlists hold.
pub(super) fn service_edit_problem(
    before: Option<&Option<Vec<u8>>>,
    now: &[u8],
    package: &str,
) -> Option<String> {
    match before {
        Some(None) => return None,
        Some(Some(old)) if old.as_slice() == now => return None,
        _ => {}
    }
    let Ok(text) = std::str::from_utf8(now) else {
        return Some("the edit leaves the _service as bytes that are not UTF-8 text".to_string());
    };
    if let Some(why) = osc::service_xml_problem(text) {
        return Some(format!("the edit leaves the _service unusable: {why}"));
    }
    // An empty or comment-only file runs no service: the run only warns about it, but an
    // edit that leaves one is refused.
    if !osc::has_services_root(text) {
        return Some("the edit leaves the _service with no <services> element".to_string());
    }
    osc::parse_service_xml(now, package)
        .err()
        .map(|why| format!("the edit leaves the _service unusable: {why}"))
}

/// The acceptance check for the checkout's `_service`: an edit that leaves it
/// malformed is rolled back the way a rejected spec is, the pre-turn copy of the
/// whole checkout included, and the rejection is noted. Returns the reason when
/// the edit was rolled back. Without a snapshot there is no pre-turn copy to put
/// back, so nothing is checked. A `_service` the snapshot held that the edit deletes is
/// refused too. `package` is the package name the service run is given (its `rpm_name`):
/// the allowlists read it for the URLs of its sources.
#[allow(clippy::too_many_arguments)]
pub(super) fn reject_bad_service(
    spec_path: &Path,
    before: &str,
    snapshot: Option<&CheckoutSnapshot>,
    step: &StepDir,
    reporter: &dyn Reporter,
    notes_path: PathBuf,
    keep: &[String],
    package: &str,
) -> Option<String> {
    let snap = snapshot?;
    let problem = match std::fs::read(spec_path.parent()?.join("_service")) {
        Ok(now) => service_edit_problem(snap.entry_of("_service").as_ref(), &now, package)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // A snapshot without a `_service` has nothing for the edit to delete.
            snap.entry_of("_service").as_ref()?;
            "the edit deletes the _service, which the package's sources come from".to_string()
        }
        Err(_) => return None,
    };
    note_spec_rejection(spec_path, &problem);
    reporter.report(&Event::SpecRejected {
        step: step.id,
        reason: ap_core::event::SpecRejection::RpmspecParseFailed {
            stderr: problem.clone(),
        },
        restored_from: spec_path.to_path_buf(),
    });
    restore_spec(spec_path, before, Some(step), reporter);
    snap.restore_keeping(reporter, Some(step), keep);
    append_notes(
        notes_path,
        &format!(
            "\n## Edit rejected — the _service is malformed\n{problem}\nThe whole edit was \
             rolled back, and _service is back as it was before the turn. Write the _service \
             XML again with every element closed (for example `</param>`), then redo the \
             rest of the edit.\n"
        ),
    );
    Some(problem)
}
