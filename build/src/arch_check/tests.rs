use super::*;
use crate::osc::{ProjectInfo, parse_project_info};

fn meta(repos: &str) -> ProjectInfo {
    parse_project_info(&format!("<project name=\"p\">{repos}</project>"))
}

fn repo(name: &str, arches: &[&str]) -> String {
    let body: String = arches
        .iter()
        .map(|a| format!("    <arch>{a}</arch>\n"))
        .collect();
    format!("<repository name=\"{name}\">\n{body}  </repository>\n")
}

fn add(repository: &str, arch: &str) -> ArchAddition {
    ArchAddition {
        repository: repository.to_string(),
        arch: arch.to_string(),
    }
}

#[test]
fn a_list_parses_in_order_without_duplicates() {
    assert_eq!(
        parse_arch_list("s390x, x86_64,s390x,,ppc64le").unwrap(),
        vec!["s390x", "x86_64", "ppc64le"]
    );
}

#[test]
fn an_unknown_or_empty_arch_list_is_refused() {
    let err = parse_arch_list("x86_64,mips").unwrap_err();
    assert!(err.contains("mips") && err.contains("s390x"), "{err}");
    assert!(parse_arch_list(" , ").is_err());
}

#[test]
fn an_arch_the_origin_builds_and_the_branch_lacks_is_added() {
    let origin = meta(&repo("ports", &["x86_64", "s390x", "armv7l"]));
    let branch = meta(&repo("ports", &["x86_64"]));
    assert_eq!(
        plan_additions(&branch, &origin, None),
        vec![add("ports", "s390x"), add("ports", "armv7l")]
    );
}

#[test]
fn an_arch_the_origin_disables_is_never_added() {
    let origin = meta(&repo("standard", &["x86_64", "i586"]));
    let branch = meta(&repo("standard", &["x86_64"]));
    assert_eq!(
        plan_additions(&branch, &origin, None),
        vec![add("standard", "i586")]
    );
    let branch = meta(&repo("standard", &["x86_64", "s390x"]));
    assert_eq!(
        plan_additions(&branch, &origin, None),
        vec![add("standard", "i586")],
        "an arch the branch has beyond the origin is kept, not added again"
    );
}

#[test]
fn a_repository_only_the_branch_has_gets_no_additions() {
    let origin = meta(&repo("standard", &["x86_64"]));
    let branch = meta(&repo("other", &["x86_64"]));
    assert!(plan_additions(&branch, &origin, None).is_empty());
}

#[test]
fn the_arches_override_narrows_additions_and_the_check_set() {
    let origin = meta(&repo("ports", &["x86_64", "s390x", "armv7l"]));
    let branch = meta(&repo("ports", &["x86_64"]));
    let wanted = parse_arch_list("s390x").unwrap();
    assert_eq!(
        plan_additions(&branch, &origin, Some(&wanted)),
        vec![add("ports", "s390x")]
    );
    let after = vec![add("ports", "s390x")];
    assert_eq!(
        check_set(&branch, &origin, &after, Some(&wanted)),
        vec![("ports".to_string(), "s390x".to_string())],
        "x86_64 is the primary build, not part of the other-arch set here"
    );
}

#[test]
fn the_check_set_is_the_origin_arches_once_added() {
    let origin = meta(&repo("ports", &["x86_64", "s390x"]));
    let branch = meta(&repo("ports", &["x86_64", "riscv64"]));
    let after = plan_additions(&branch, &origin, None);
    assert_eq!(after, vec![add("ports", "s390x")]);
    assert_eq!(
        check_set(&branch, &origin, &after, None),
        vec![
            ("ports".to_string(), "s390x".to_string()),
            ("ports".to_string(), "x86_64".to_string()),
        ],
        "a branch arch the origin does not build for the repository is not checked"
    );
    let other = meta(&repo("mine", &["x86_64", "riscv64"]));
    assert_eq!(
        check_set(&other, &origin, &[], None).len(),
        2,
        "a repository the origin lacks keeps the branch's own arches"
    );
}

#[test]
fn an_added_arch_lands_after_the_existing_ones() {
    let xml = format!(
        "<project name=\"home:u:p\">\n  {}</project>\n",
        repo("ports", &["x86_64"]).replace('\n', "\n  ")
    );
    let (edited, made) = add_arches_to_meta(&xml, &[add("ports", "s390x")]).unwrap();
    assert_eq!(made, vec![add("ports", "s390x")]);
    let info = parse_project_info(&edited);
    assert_eq!(repo_arches(&info, "ports"), vec!["x86_64", "s390x"]);
    assert!(edited.contains("    <arch>s390x</arch>"), "{edited}");
}

#[test]
fn adding_an_arch_twice_changes_nothing() {
    let xml = format!(
        "<project name=\"home:u:p\">{}</project>",
        repo("ports", &["x86_64"])
    );
    let (once, _) = add_arches_to_meta(&xml, &[add("ports", "s390x")]).unwrap();
    let (twice, made) = add_arches_to_meta(&once, &[add("ports", "s390x")]).unwrap();
    assert!(made.is_empty());
    assert_eq!(once, twice);
}

#[test]
fn a_self_closing_repository_gets_its_arch_in_a_body() {
    let xml = "<project name=\"home:u:p\"><repository name=\"ports\"/></project>";
    let (edited, made) = add_arches_to_meta(xml, &[add("ports", "s390x")]).unwrap();
    assert_eq!(made.len(), 1);
    assert_eq!(
        repo_arches(&parse_project_info(&edited), "ports"),
        vec!["s390x"]
    );
}

#[test]
fn a_repository_the_meta_lacks_is_an_error() {
    let xml = "<project name=\"home:u:p\"></project>";
    assert!(add_arches_to_meta(xml, &[add("ports", "s390x")]).is_err());
}

/// A branch project meta held in memory. Every read and write is recorded in
/// `events`, with `read` marked when the project-meta lock was not held.
#[derive(Default)]
struct FakeMeta {
    xml: std::cell::RefCell<String>,
    events: std::cell::RefCell<Vec<String>>,
}

impl FakeMeta {
    fn with(xml: &str) -> Self {
        let fake = FakeMeta::default();
        *fake.xml.borrow_mut() = xml.to_string();
        fake
    }
}

impl ArchObs for FakeMeta {
    fn project_meta(&self, _project: &str) -> Result<Option<String>, String> {
        let locked = !crate::osc::project_meta_lock_is_free();
        let mark = if locked { "read" } else { "read-unlocked" };
        self.events.borrow_mut().push(mark.to_string());
        Ok(Some(self.xml.borrow().clone()))
    }

    fn set_project_meta(&self, _project: &str, xml: &str) -> Result<(), String> {
        self.events.borrow_mut().push("write".to_string());
        *self.xml.borrow_mut() = xml.to_string();
        Ok(())
    }
}

#[test]
fn only_a_home_project_may_be_written_to() {
    assert!(ensure_branch_project("home:u:branches:X11:Wayland").is_ok());
    assert!(ensure_branch_project("X11:Wayland").is_err());
    assert!(ensure_branch_project("devel:languages:rust").is_err());
    assert!(ensure_branch_project("homer:x").is_err());
    // The edit refuses before any OBS call: the project is not a home one.
    let fake = FakeMeta::with(&repo_xml("ports", &["x86_64"]));
    let err = edit_branch_meta(
        &fake,
        "openSUSE:Factory",
        |_| vec![add("ports", "s390x")],
        |_| {},
    )
    .unwrap_err();
    assert!(err.contains("not a home: project"), "{err}");
    assert!(fake.events.borrow().is_empty());
}

fn repo_xml(name: &str, arches: &[&str]) -> String {
    format!(
        "<project name=\"home:u:p\">{}</project>",
        repo(name, arches)
    )
}

#[test]
fn the_edit_reads_announces_then_writes_under_the_lock() {
    let fake = FakeMeta::with(&repo_xml("ports", &["x86_64"]));
    let announced = std::cell::RefCell::new(Vec::new());
    let made = edit_branch_meta(
        &fake,
        "home:u:p",
        |info| {
            assert_eq!(repo_arches(info, "ports"), vec!["x86_64"]);
            vec![add("ports", "s390x")]
        },
        |adds| {
            announced
                .borrow_mut()
                .extend(adds.iter().map(|a| format!("{}/{}", a.repository, a.arch)));
            fake.events.borrow_mut().push("announce".to_string());
        },
    )
    .unwrap();
    assert_eq!(made, vec![add("ports", "s390x")]);
    assert_eq!(*announced.borrow(), ["ports/s390x"]);
    assert_eq!(*fake.events.borrow(), ["read", "announce", "write"]);
    assert_eq!(
        repo_arches(&parse_project_info(&fake.xml.borrow()), "ports"),
        vec!["x86_64", "s390x"]
    );
}

#[test]
fn a_second_edit_reads_then_changes_nothing() {
    let fake = FakeMeta::with(&repo_xml("ports", &["x86_64", "s390x"]));
    let made =
        edit_branch_meta(&fake, "home:u:p", |_| vec![add("ports", "s390x")], |_| {}).unwrap();
    assert!(made.is_empty());
    assert_eq!(*fake.events.borrow(), ["read"]);
}

#[test]
fn a_comment_that_looks_like_an_arch_or_a_repository_is_not_one() {
    let xml = "<project name=\"home:u:p\">\n  <!-- <repository name=\"ports\"> -->\n  \
               <repository name=\"ports\">\n    <!-- <arch>s390x</arch> -->\n    \
               <arch>x86_64</arch>\n  </repository>\n</project>";
    let (edited, made) = add_arches_to_meta(xml, &[add("ports", "s390x")]).unwrap();
    assert_eq!(made.len(), 1);
    // The comment's own text is kept; the one added arch is the second copy.
    assert_eq!(edited.matches("<arch>s390x</arch>").count(), 2);
    assert!(edited.contains("<!-- <arch>s390x</arch> -->"));
    assert_eq!(
        repo_arches(&parse_project_info(&edited), "ports"),
        vec!["x86_64", "s390x"]
    );
}
