use super::*;
use ap_core::sink::NullReporter;

const BRANCH: &str = "home:u:branches:Base:System";

fn meta(name: &str, repos: &[(&str, &[(&str, &str)])]) -> String {
    let mut xml = format!("<project name=\"{name}\">\n");
    for (repo, paths) in repos {
        xml.push_str(&format!("  <repository name=\"{repo}\">\n"));
        for (p, r) in *paths {
            xml.push_str(&format!("    <path project=\"{p}\" repository=\"{r}\"/>\n"));
        }
        xml.push_str("    <arch>x86_64</arch>\n  </repository>\n");
    }
    xml.push_str("</project>\n");
    xml
}

/// Metas of the branch, Base:System and Factory; repository listings by
/// `project|repository`. A repository with no listing fails to list.
struct Listed {
    names: HashMap<String, Vec<String>>,
}

impl Obs for Listed {
    fn project_meta(&self, p: &str) -> Result<Option<String>, String> {
        Ok(match p {
            BRANCH => Some(meta(
                BRANCH,
                &[(
                    "openSUSE_Tumbleweed",
                    &[("Base:System", "openSUSE_Tumbleweed")],
                )],
            )),
            "Base:System" => Some(meta(
                "Base:System",
                &[("openSUSE_Tumbleweed", &[("openSUSE:Factory", "snapshot")])],
            )),
            "openSUSE:Factory" => Some(meta("openSUSE:Factory", &[("snapshot", &[])])),
            _ => None,
        })
    }
    fn set_project_meta(&self, _: &str, _: &str) -> Result<(), String> {
        Ok(())
    }
    fn published_binaries(&self, _: &[&str]) -> Result<Vec<PublishedBinary>, String> {
        Ok(Vec::new())
    }
    fn package_meta(&self, _: &str, _: &str) -> Result<Option<String>, String> {
        Ok(None)
    }
    fn rpms_containing(&self, _: &str, _: &[String], _: &str) -> Result<osc::RpmSearch, String> {
        Ok(osc::RpmSearch::default())
    }
    fn rpm_provides(&self, _: &str, _: &str, _: &str, _: &str) -> Result<Vec<String>, String> {
        Ok(Vec::new())
    }
    fn repository_names(
        &self,
        project: &str,
        repository: &str,
        _: &str,
    ) -> Result<Vec<String>, String> {
        self.names
            .get(&format!("{project}|{repository}"))
            .cloned()
            .ok_or_else(|| format!("cannot list {project}/{repository}"))
    }
    fn project_packages(&self, _: &str) -> Result<HashSet<String>, String> {
        Ok(HashSet::new())
    }
}

fn resolver(listings: &[(&str, &[&str])]) -> DepResolver {
    let names = listings
        .iter()
        .map(|(k, v)| (k.to_string(), v.iter().map(|n| n.to_string()).collect()))
        .collect();
    let reporter: Arc<dyn Reporter + Send + Sync> = Arc::new(NullReporter);
    DepResolver::new(Box::new(Listed { names }), reporter, BRANCH, "x86_64")
}

#[test]
fn a_name_a_build_path_repository_lists_is_in_the_build_path() {
    // Factory lists autoconf though the search would not return it.
    let r = resolver(&[
        ("Base:System|openSUSE_Tumbleweed", &[]),
        ("openSUSE:Factory|snapshot", &["autoconf", "automake"]),
    ]);
    assert_eq!(r.in_build_path("autoconf"), Ok(true));
}

#[test]
fn a_name_no_listing_has_is_missing_from_the_build_path() {
    let r = resolver(&[
        ("Base:System|openSUSE_Tumbleweed", &["bison"]),
        ("openSUSE:Factory|snapshot", &["autoconf"]),
    ]);
    assert_eq!(r.in_build_path("nothing-publishes-this"), Ok(false));
}

#[test]
fn a_failed_listing_that_hides_the_name_is_unknown_not_missing() {
    // Factory cannot be listed, so the name may be there.
    let r = resolver(&[("Base:System|openSUSE_Tumbleweed", &[])]);
    assert!(r.in_build_path("autoconf").is_err());
}

#[test]
fn a_listed_name_is_found_even_when_another_listing_fails() {
    let r = resolver(&[("Base:System|openSUSE_Tumbleweed", &["autoconf"])]);
    assert_eq!(r.in_build_path("autoconf"), Ok(true));
}

#[test]
fn a_name_is_reported_once_per_run() {
    let r = resolver(&[]);
    assert!(r.first_report("gcc"));
    assert!(!r.first_report("gcc"));
    assert!(r.first_report("autoconf"));
}
