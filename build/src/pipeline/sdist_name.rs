//! The file name a downloaded PyPI sdist is saved under in the checkout.
//!
//! PyPI names the file as its uploader did, which can differ in case from the
//! name a spec's `Source` gives it: `amqpstorm-3.1.4.tar.gz` for a spec that
//! says `AMQPStorm-%{version}.tar.gz`. OBS builds the spec's name, so the
//! checkout must hold the file under it.

/// The name to save PyPI's sdist `pypi_name` under: the spelling the spec's
/// `Source` lines give it, when they differ from PyPI's only in case. Any other
/// name is PyPI's own.
pub(crate) fn saved_sdist_name(spec: &str, pypi_name: &str) -> String {
    crate::spec::expanded_sources(spec)
        .into_iter()
        .map(|s| s.file)
        .find(|f| f != pypi_name && f.eq_ignore_ascii_case(pypi_name))
        .unwrap_or_else(|| pypi_name.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPEC: &str = "Name: python-AMQPStorm\nVersion: 3.1.4\nRelease: 0\n\
Source: https://files.pythonhosted.org/packages/source/A/AMQPStorm/AMQPStorm-%{version}.tar.gz\n";

    #[test]
    fn the_spec_spelling_wins_over_pypis_when_only_the_case_differs() {
        assert_eq!(
            saved_sdist_name(SPEC, "amqpstorm-3.1.4.tar.gz"),
            "AMQPStorm-3.1.4.tar.gz"
        );
    }

    #[test]
    fn a_name_the_spec_does_not_give_is_pypis_own() {
        assert_eq!(
            saved_sdist_name(SPEC, "other-3.1.4.tar.gz"),
            "other-3.1.4.tar.gz"
        );
        assert_eq!(
            saved_sdist_name("Name: x\nVersion: 1\n", "x-1.tar.gz"),
            "x-1.tar.gz"
        );
    }

    #[test]
    fn a_rename_in_the_spec_is_the_name_to_save() {
        let spec = "Name: x\nVersion: 1\nRelease: 0\n\
Source0: https://example.org/archive/v1.tar.gz#/X-1.tar.gz\n";
        assert_eq!(saved_sdist_name(spec, "x-1.tar.gz"), "X-1.tar.gz");
    }
}
