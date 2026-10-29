use super::{license_guard_overrides, unverified_license_guards};
use crate::spec::checks::*;

/// torchcodec: the guard asks whether the FFmpeg linked is licence-compatible;
/// a spec that says what was checked builds, one that does not, or one that
/// accepts terms, stops.
#[test]
fn a_confirmation_backed_by_a_check_is_allowed_but_terms_never_are() {
    let guard = "export I_CONFIRM_THIS_IS_NOT_A_LICENSE_VIOLATION=1\n";
    assert_eq!(unverified_license_guards(guard).len(), 1);
    let ok = format!(
        "# licence-guard-verified: ffmpeg-8 spec configures without --enable-gpl/--enable-nonfree (LGPL-2.1+)\n{guard}"
    );
    assert!(unverified_license_guards(&ok).is_empty());
    // Too little to be a check.
    let thin = format!("# licence-guard-verified: ok\n{guard}");
    assert_eq!(unverified_license_guards(&thin).len(), 1);
    // Accepting terms is a person's decision, whatever the comment says.
    let eula = format!("{ok}ACCEPT_EULA=Y make\n");
    let left = unverified_license_guards(&eula);
    assert_eq!(left.len(), 1);
    assert!(left[0].1.contains("ACCEPT_EULA"));
}

#[test]
fn a_variable_that_confirms_a_licence_is_found_outside_comments() {
    let spec = "%build
# I_CONFIRM_THIS_IS_NOT_A_LICENSE_VIOLATION=1 would bypass it
                    export I_CONFIRM_THIS_IS_NOT_A_LICENSE_VIOLATION=1
                    ACCEPT_EULA=Y make
                    export CMAKE_ARGS=\"-DLICENSE_FILE=x\"
%changelog
- ACCEPT_EULA=Y\n";
    let found = license_guard_overrides(spec);
    assert_eq!(found.len(), 2, "{found:?}");
    assert_eq!(found[0].0, 3);
    assert_eq!(found[1].1, "ACCEPT_EULA=Y make");
}
