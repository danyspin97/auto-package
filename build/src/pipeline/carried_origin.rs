//! The recipe of a dependency that is bumped to a new version. A dependency is
//! not an update, so it has no update mode, but its recipe is still the one its
//! origin ships: a problem the origin's spec already has is inherited, reported
//! and not fixed by this version (see `update_mode::split_inherited`).

use super::*;
use crate::pipeline::update_mode::UpdateMode;
use std::collections::BTreeSet;
use std::path::Path;

/// The spec a bumped dependency's recipe is checked against: the origin's
/// (`update_mode::origin_base`), else, for a package that is not a link, the
/// revision the checkout started from (`.osc/_files`).
pub(super) fn origin_spec(
    project: &str,
    obs_name: &str,
    checkout_dir: &Path,
    reporter: &dyn Reporter,
) -> Option<String> {
    if let Some(spec) = update_mode::origin_base(project, obs_name, reporter).and_then(|o| o.spec) {
        return Some(spec);
    }
    let rev = osc::checkout_revision(checkout_dir)?;
    osc::cat_file_at_revision(
        project,
        obs_name,
        &format!("{obs_name}.spec"),
        &rev,
        Emit::run(reporter),
    )
}

/// The mode a bumped dependency's run checks inherited problems in: `Some` when
/// its recipe was kept (`kept`) and its origin's spec is known. A root update
/// has its own mode (`update_mode::update_mode`), so this is `None` for it.
pub(super) fn carried_mode(
    origin_spec: Option<String>,
    kept: bool,
    update: bool,
) -> Option<UpdateMode> {
    let spec = origin_spec.filter(|_| kept && !update)?;
    Some(UpdateMode {
        origin_spec: Some(spec),
        reported: BTreeSet::new(),
    })
}

/// The mode the inherited problems are checked in: a root update's, else a
/// bumped dependency's (`carried`).
pub(super) fn inherit_mode(st: &mut RunState) -> Option<&mut UpdateMode> {
    st.update.as_mut().or(st.carried.as_mut())
}
