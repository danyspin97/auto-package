//! The task text of a PatchReview turn: a fresh look at one patch that another
//! turn rebased, wrote or edited. Kept apart from build_prompt.rs.

/// What a [`super::Task::PatchReview`] turn is told to do. The patch, what
/// changed in it and quilt's verdict on it come in the Context.
pub(super) fn patch_review_task() -> &'static str {
    "You are reviewing ONE patch, named under \"Context\", that another turn rebased onto \
     a new release, wrote or edited. You did not write it: judge it on its own. Does it \
     still do what the original did: the same intent, no hunk dropped or half-ported, no \
     unrelated change, nothing that hides a failure (a test deleted, `|| true`, a check \
     switched off), the header kept? The new sources are under `source/`; read the files \
     it touches there.\n\
     \n\
     Give your verdict in the `review` field:\n\
     - `accept` when the patch is right.\n\
     - `rewrite` when you can fix it. Do it with quilt in `source/`: `quilt push` up to \
       the patch, edit, then `quilt refresh -p ab --no-timestamps --no-index` (`-p 0` \
       instead of `-p ab` for a `-p0` patch). Never write the `.patch` file by hand, and \
       keep its header and its `# PATCH-...` comment.\n\
     - `reject` when it is wrong and you cannot fix it, with each problem in `problems`: \
       the patch is then rebased again, told your reasons.\n\
     Set `action` to `edited` for a rewrite and to `no_change` for accept and reject. Set \
     `action` to `blocked`, with `why`, only when you cannot judge (an unreadable file, a \
     patch you cannot follow).\n\
     \n\
     You may change only the named patch file. The spec, `_service`, `.changes` and every \
     other patch are refused, and the pipeline rolls them back. The same Bash rules as a \
     patch rebase hold: one `cd source && quilt <sub>` per call, no chaining, no \
     environment variables. Quilt cannot create, delete or rename a patch here. A file that \
     Edit and Write refuse as not UTF-8 cannot be edited in this turn: do not try."
}
