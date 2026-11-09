//! The refresh-patches prompt and convention 111 tell the agent every removal
//! proof the pipeline accepts, the one for a patch whose files are all gone
//! included.

use crate::agent::outcome::refresh_patches_task;

#[test]
fn the_refresh_prompt_names_the_targets_absent_proof() {
    let prompt = refresh_patches_task();
    assert!(
        prompt.contains(
            "Or, when every file the patch changes is absent from the new archive (quilt says \
             `can't find file to patch` for every hunk): remove the patch; the pipeline checks \
             it itself and never accepts it for a security patch."
        ),
        "{prompt}"
    );
    assert!(
        prompt.contains("A change already upstream with different surrounding text is removed too"),
        "{prompt}"
    );
}

#[test]
fn convention_111_names_the_targets_absent_proof() {
    let text = include_str!("../../../conventions/111.md");
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        flat.contains(
            "every file the patch changes is gone from the archive: the patch is removed, \
             never a security patch."
        ),
        "{flat}"
    );
    assert!(
        flat.contains("A change already upstream with different surrounding text is removed too"),
        "{flat}"
    );
}
