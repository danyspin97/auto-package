//! Where a review is kept: plain files under
//! `<work-dir>/reviews/<request-id>/<run-id>/`, nothing else of the work
//! directory.

use std::path::{Path, PathBuf};

use super::run::Reviewed;

/// The directory of one review.
pub fn review_dir(work_dir: &Path, request_id: &str, run_id: &str) -> PathBuf {
    work_dir.join("reviews").join(request_id).join(run_id)
}

/// The name of the diff file of action `index` of `count`. The index keeps
/// two actions on one package name apart.
pub fn diff_name(count: usize, index: usize, package: &str) -> String {
    if count == 1 {
        "source.diff".to_string()
    } else {
        format!("source-{index}-{package}.diff")
    }
}

/// Write the report and what it was made from into `dir`: `review.txt`,
/// `review.json`, `request.xml` and the source diffs.
pub fn save(dir: &Path, reviewed: &Reviewed, text: &str, json: &str) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    std::fs::write(dir.join("review.txt"), text)?;
    std::fs::write(dir.join("review.json"), json)?;
    std::fs::write(dir.join("request.xml"), &reviewed.inputs.request_xml)?;
    let count = reviewed.inputs.actions.len();
    for (i, a) in reviewed.inputs.actions.iter().enumerate() {
        if let Some(diff) = &a.diff {
            std::fs::write(dir.join(diff_name(count, i, &a.target.package)), diff)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::Severity;
    use super::super::gather_tests::obs;
    use super::super::run::{Deps, Options, review};
    use super::*;

    #[test]
    fn the_review_dir_is_under_reviews_and_the_request_id() {
        let d = review_dir(Path::new("ap-work"), "1394662", "20261103-120102-ab12");
        assert_eq!(d, Path::new("ap-work/reviews/1394662/20261103-120102-ab12"));
    }

    #[test]
    fn two_actions_on_one_package_name_get_two_diff_files() {
        assert_eq!(diff_name(1, 0, "p"), "source.diff");
        assert_ne!(diff_name(2, 0, "p"), diff_name(2, 1, "p"));
    }

    #[test]
    fn a_review_is_saved_as_plain_files() {
        let scratch = tempfile::tempdir().unwrap();
        let o = Options {
            id: "1394662",
            download: false,
            scratch: scratch.path(),
            fail_on: Severity::Warning,
            fail_on_agent: false,
            build: "t",
        };
        let plan = |_: &super::super::gather::ActionInputs| Err("off".to_string());
        let deps = Deps {
            plan: &plan,
            repology: &|_| Err("off".into()),
            ldconfig_macro: &|_| true,
            host_allowed: &|_| true,
        };
        let reviewed = review(&obs(), &o, &deps).unwrap();
        let work = tempfile::tempdir().unwrap();
        let dir = review_dir(work.path(), "1394662", "r1");
        save(&dir, &reviewed, "text\n", "{}\n").unwrap();
        let names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        for want in ["review.txt", "review.json", "request.xml", "source.diff"] {
            assert!(names.contains(&want.to_string()), "{want} in {names:?}");
        }
        assert!(
            std::fs::read_to_string(dir.join("request.xml"))
                .unwrap()
                .contains("cracklib")
        );
    }
}
