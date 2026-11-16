//! The journal of rejected spec edits: how many times one spec was rejected with
//! the same error, so that a turn which repeats it fails instead of asking again.

use super::*;

/// How many times the same validation error rejected an edit of one spec, in
/// this process. A fourth identical turn would be re-prompting for the same
/// failure (python-torch: eleven rejections of one inherited patch, $2.64).
type SpecRejections = std::collections::HashMap<(PathBuf, u64), (u32, String)>;

static SPEC_REJECTIONS: std::sync::LazyLock<std::sync::Mutex<SpecRejections>> =
    std::sync::LazyLock::new(Default::default);

/// Identical rejections of one spec after which the step fails instead of asking again.
const MAX_IDENTICAL_REJECTIONS: u32 = 3;

pub(in crate::pipeline) fn note_spec_rejection(spec_path: &Path, problem: &str) {
    let mut map = SPEC_REJECTIONS.lock().unwrap_or_else(|e| e.into_inner());
    let entry = map
        .entry((spec_path.to_path_buf(), hash_spec(problem)))
        .or_insert((0, problem.to_string()));
    entry.0 += 1;
}

pub(in crate::pipeline) fn clear_spec_rejections(spec_path: &Path) {
    let mut map = SPEC_REJECTIONS.lock().unwrap_or_else(|e| e.into_inner());
    map.retain(|(p, _), _| p != spec_path);
}

/// The failure for a spec rejected with the same error [`MAX_IDENTICAL_REJECTIONS`]
/// times, or `None` while another try is still reasonable.
pub(in crate::pipeline) fn repeated_spec_rejection(spec_path: &Path) -> Option<String> {
    let map = SPEC_REJECTIONS.lock().unwrap_or_else(|e| e.into_inner());
    map.iter()
        .find(|((p, _), (n, _))| p == spec_path && *n >= MAX_IDENTICAL_REJECTIONS)
        .map(|(_, (n, problem))| {
            let first: String = problem
                .lines()
                .next()
                .unwrap_or("")
                .chars()
                .take(300)
                .collect();
            format!(
                "the spec was rejected {n} times with the same error and the agent does not \
                 get past it: {first}"
            )
        })
}
