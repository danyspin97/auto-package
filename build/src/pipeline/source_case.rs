//! A checkout file that a spec's `Source` names in another case.
//!
//! PyPI reports a sdist under the name its uploader gave it, and a spec may
//! spell the same file in another case. The checkout then holds the file the
//! spec does not name, and OBS builds the spec's name, so the file is renamed.

use std::path::Path;

/// Rename each file of `checkout_dir` that a `Source` of `spec` names in another
/// case to the spec's spelling. Returns the `(from, to)` pairs renamed. A
/// source whose exact name is there, or that has no case-only match, is left.
pub(crate) fn align_source_case(
    spec: &str,
    checkout_dir: &Path,
) -> Result<Vec<(String, String)>, String> {
    let mut renamed = Vec::new();
    let Ok(entries) = std::fs::read_dir(checkout_dir) else {
        return Ok(renamed);
    };
    let names: Vec<String> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_ok_and(|t| t.is_file()))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    for source in crate::spec::expanded_sources(spec) {
        let wanted = source.file;
        if names.iter().any(|n| *n == wanted) {
            continue;
        }
        let Some(found) = names
            .iter()
            .find(|n| n.eq_ignore_ascii_case(&wanted) && **n != wanted)
        else {
            continue;
        };
        std::fs::rename(checkout_dir.join(found), checkout_dir.join(&wanted))
            .map_err(|e| format!("renaming {found} to {wanted}: {e}"))?;
        renamed.push((found.clone(), wanted));
    }
    Ok(renamed)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPEC: &str = "Name: python-AMQPStorm\nVersion: 3.1.4\nRelease: 0\n\
Source: https://files.pythonhosted.org/packages/source/A/AMQPStorm/AMQPStorm-%{version}.tar.gz\n";

    #[test]
    fn the_checkout_copy_takes_the_spec_spelling() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("amqpstorm-3.1.4.tar.gz"), "x").unwrap();
        let renamed = align_source_case(SPEC, dir.path()).unwrap();
        assert_eq!(
            renamed,
            vec![(
                "amqpstorm-3.1.4.tar.gz".to_string(),
                "AMQPStorm-3.1.4.tar.gz".to_string()
            )]
        );
        assert!(dir.path().join("AMQPStorm-3.1.4.tar.gz").is_file());
        assert!(!dir.path().join("amqpstorm-3.1.4.tar.gz").exists());
    }

    #[test]
    fn a_file_already_under_the_spec_name_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("AMQPStorm-3.1.4.tar.gz"), "x").unwrap();
        assert!(align_source_case(SPEC, dir.path()).unwrap().is_empty());
        assert!(dir.path().join("AMQPStorm-3.1.4.tar.gz").is_file());
    }

    #[test]
    fn a_different_name_is_not_a_case_match() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("other-3.1.4.tar.gz"), "x").unwrap();
        assert!(align_source_case(SPEC, dir.path()).unwrap().is_empty());
        assert!(dir.path().join("other-3.1.4.tar.gz").is_file());
    }
}
