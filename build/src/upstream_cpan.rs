//! The newest version of a CPAN distribution, from MetaCPAN's record of its
//! latest release. Asked for a `perl-` package, or one whose Source or Url is
//! on a CPAN host, after Repology has had its say (see
//! [`crate::update::ecosystems`]).

use crate::sources::{archive_name_version, last_segment};

/// Whether `name` is a distribution name as MetaCPAN spells it in its API path
/// (`DBI`, `DBIx-MyServer`): letters, digits and `-` only.
pub fn is_dist_name(name: &str) -> bool {
    !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// Whether `url` is on a CPAN host (`cpan.org`, `search.cpan.org`,
/// `metacpan.org`, ...).
pub fn is_cpan_url(url: &str) -> bool {
    let Some(rest) = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
    else {
        return false;
    };
    let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
    ["cpan.org", "metacpan.org"]
        .iter()
        .any(|d| crate::discovery::host_is_under(host, d))
}

/// The newest release MetaCPAN lists for the distribution `dist`, from its
/// latest-release document. The host must pass the shared allowlist for source
/// fetches.
pub fn latest_on_metacpan(dist: &str) -> Result<String, String> {
    if !is_dist_name(dist) {
        return Err(format!("{dist:?} is not a CPAN distribution name"));
    }
    let host = "fastapi.metacpan.org";
    crate::allowed_hosts::shared()
        .check_host(
            host,
            crate::allowed_hosts::Use::Source,
            &crate::allowed_hosts::Asker::source(dist),
        )
        .map_err(|e| format!("{host}: {e}"))?;
    let url = format!("https://{host}/v1/release/{dist}");
    newest_on_metacpan(&crate::upstream::get_text(&url)?)
}

/// The `version` of the release a MetaCPAN release document names as latest.
fn newest_on_metacpan(body: &str) -> Result<String, String> {
    let doc: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("MetaCPAN sent no JSON: {e}"))?;
    if doc["status"] != "latest" {
        return Err("MetaCPAN names no latest release for it".to_string());
    }
    doc["version"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| "MetaCPAN names no version for it".to_string())
}

/// The CPAN distribution a package builds, when its name or its Source or Url
/// says so. The Source archive's name is preferred, as it keeps the
/// distribution's own spelling: `perl-DBIx-MyServer` with the Source
/// `DBIx-MyServer-1.2.tar.gz` is `DBIx-MyServer`.
pub fn dist_of(header: &crate::spec::Header) -> Option<String> {
    let source = header.expanded_source.as_deref().unwrap_or_default();
    let url = header.url.as_deref().unwrap_or_default();
    let perl_name = header.name.strip_prefix("perl-");
    if perl_name.is_none() && !is_cpan_url(source) && !is_cpan_url(url) {
        return None;
    }
    let from_archive = archive_name_version(last_segment(source))
        .map(|(name, _)| name)
        .filter(|name| is_dist_name(name) && !name.starts_with("perl-"));
    let from_name = perl_name.unwrap_or(&header.name);
    from_archive.or_else(|| is_dist_name(from_name).then(|| from_name.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(spec: &str) -> crate::spec::Header {
        crate::spec::parse_header(spec).expect("a header with Name and Version")
    }

    #[test]
    fn metacpan_names_the_version_of_its_latest_release() {
        assert_eq!(
            newest_on_metacpan(r#"{"version":"1.656","status":"latest"}"#).as_deref(),
            Ok("1.656")
        );
        assert!(newest_on_metacpan(r#"{"version":"1.657","status":"cpan"}"#).is_err());
        assert!(newest_on_metacpan(r#"{"name":"DBI-1.656","status":"latest"}"#).is_err());
        assert!(newest_on_metacpan("<html>").is_err());
        // A malformed distribution name is refused before anything is fetched.
        assert!(latest_on_metacpan("../etc").is_err());
        assert!(latest_on_metacpan("DBI.pm").is_err());
        assert!(latest_on_metacpan("").is_err());
    }

    #[test]
    fn a_distribution_name_is_letters_digits_and_dashes() {
        assert!(is_dist_name("DBIx-MyServer"));
        assert!(!is_dist_name("a_b"));
        assert!(!is_dist_name("a.b"));
        assert!(!is_dist_name(""));
    }

    #[test]
    fn a_cpan_package_is_read_from_its_name_or_its_source() {
        assert_eq!(
            dist_of(&header(
                "Name: perl-DBIx-MyServer\nVersion: 0.5\nSource: DBIx-MyServer-%{version}.tar.gz\n"
            ))
            .as_deref(),
            Some("DBIx-MyServer")
        );
        assert_eq!(
            dist_of(&header(
                "Name: foo\nVersion: 1.0\nSource: https://www.cpan.org/modules/by-module/Foo/Foo-Bar-%{version}.tar.gz\n"
            ))
            .as_deref(),
            Some("Foo-Bar")
        );
        // Not a CPAN package: a C library whose name is only a tarball name.
        assert_eq!(
            dist_of(&header(
                "Name: galera-4\nVersion: 26.4\nSource: galera-%{version}.tar.gz\n"
            )),
            None
        );
    }
}
