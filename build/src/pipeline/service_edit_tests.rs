use super::service_edit::{reject_bad_service, service_edit_problem};
use super::snapshot::CheckoutSnapshot;
use ap_core::run::StepDir;
use ap_core::sink::NullReporter;

const PKG: &str = "foo";

const GOOD: &[u8] = b"<services>\n  <service mode=\"manual\" name=\"set_version\"/>\n</services>\n";

/// The `_service` of the failed run: a `<param>` that is never closed.
const MALFORMED: &[u8] = b"<services>\n  <service name=\"obs_scm\">\n    <param name=\"update\">false\n  </service>\n</services>\n";

#[test]
fn a_service_the_edit_left_alone_is_not_judged_even_when_malformed() {
    let before = Some(MALFORMED.to_vec());
    assert_eq!(service_edit_problem(Some(&before), MALFORMED, "foo"), None);
}

#[test]
fn a_service_the_edit_makes_malformed_is_refused_with_the_line() {
    let before = Some(GOOD.to_vec());
    let why = service_edit_problem(Some(&before), MALFORMED, "foo").expect("malformed");
    assert!(why.contains("not well-formed XML"), "{why}");
    assert!(why.contains("line"), "{why}");
}

#[test]
fn a_well_formed_edit_of_the_service_is_kept() {
    let before = Some(MALFORMED.to_vec());
    assert_eq!(service_edit_problem(Some(&before), GOOD, "foo"), None);
    assert_eq!(service_edit_problem(None, GOOD, "foo"), None);
}

#[test]
fn a_malformed_service_the_edit_creates_is_refused() {
    let why = service_edit_problem(None, MALFORMED, "foo").expect("malformed");
    assert!(why.contains("_service"), "{why}");
}

#[test]
fn a_service_whose_bytes_were_not_kept_cannot_be_compared_so_it_is_not_refused() {
    assert_eq!(service_edit_problem(Some(&None), MALFORMED, "foo"), None);
}

#[test]
fn a_service_that_is_not_utf8_is_refused() {
    let before = Some(GOOD.to_vec());
    let why = service_edit_problem(Some(&before), &[0xff, 0xfe, b'<'], "foo").expect("not utf-8");
    assert!(why.contains("UTF-8"), "{why}");
}

fn refused(now: &[u8]) -> String {
    let before = Some(GOOD.to_vec());
    service_edit_problem(Some(&before), now, PKG).expect("refused")
}

#[test]
fn an_empty_or_rootless_service_is_refused() {
    assert!(refused(b"").contains("no <services>"));
    assert!(refused(b" \n\t\n").contains("no <services>"));
    assert!(refused(b"<!-- nothing to run -->\n").contains("no <services>"));
}

#[test]
fn two_roots_and_an_unclosed_root_are_refused() {
    assert!(refused(b"<services/>\n<services/>\n").contains("unexpected markup"));
    let unclosed = b"<services>\n  <service name=\"set_version\"/>\n";
    assert!(refused(unclosed).contains("unclosed element"));
}

#[test]
fn a_disallowed_service_is_refused_by_the_allowlist() {
    let evil = b"<services>\n  <service name=\"evil_service\"/>\n</services>\n";
    assert!(refused(evil).contains("not allowed"), "{}", refused(evil));
}

#[test]
fn a_bom_comments_and_an_ascii_iso_declaration_are_accepted() {
    let bom =
        "\u{feff}<services>\n  <service mode=\"manual\" name=\"set_version\"/>\n</services>\n";
    assert_eq!(service_edit_problem(None, bom.as_bytes(), PKG), None);
    let commented = b"<?xml version=\"1.0\" encoding=\"ISO-8859-1\"?>\n<!-- note -->\n<services>\n  <service name=\"set_version\"/>\n</services>\n";
    assert_eq!(service_edit_problem(None, commented, PKG), None);
}

#[test]
fn a_declared_non_utf8_file_is_refused_as_the_parser_reads_utf8_only() {
    // ISO-8859-1 with a non-ASCII byte: the services are read as UTF-8, as run_service does.
    let latin1 = b"<?xml version=\"1.0\" encoding=\"ISO-8859-1\"?>\n<services>\n  <service name=\"set_version\"><!-- caf\xe9 --></service>\n</services>\n";
    assert!(refused(latin1).contains("UTF-8"), "{}", refused(latin1));
}

#[test]
fn a_services_element_alone_is_fine_and_an_empty_root_is_refused() {
    assert_eq!(service_edit_problem(None, b"<services/>\n", PKG), None);
    assert_eq!(
        service_edit_problem(None, b"<services></services>\n", PKG),
        None
    );
}

#[test]
fn a_service_the_edit_deletes_is_refused_and_put_back() {
    let root = tempfile::tempdir().unwrap();
    let spec = root.path().join("foo.spec");
    std::fs::write(&spec, "Name: foo\n").unwrap();
    std::fs::write(root.path().join("_service"), GOOD).unwrap();
    let snap = CheckoutSnapshot::take(root.path());
    std::fs::remove_file(root.path().join("_service")).unwrap();
    let step = StepDir {
        path: root.path().join("step"),
        id: ap_core::event::StepId(1),
    };
    std::fs::create_dir_all(&step.path).unwrap();
    let why = reject_bad_service(
        &spec,
        "Name: foo\n",
        Some(&snap),
        &step,
        &NullReporter,
        root.path().join("notes.md"),
        &[],
        PKG,
    );
    assert!(why.is_some_and(|w| w.contains("deletes the _service")));
    assert_eq!(std::fs::read(root.path().join("_service")).unwrap(), GOOD);
}

#[test]
fn a_service_absent_before_and_after_the_edit_is_not_refused() {
    let root = tempfile::tempdir().unwrap();
    let spec = root.path().join("foo.spec");
    std::fs::write(&spec, "Name: foo\n").unwrap();
    let snap = CheckoutSnapshot::take(root.path());
    let step = StepDir {
        path: root.path().join("step"),
        id: ap_core::event::StepId(1),
    };
    std::fs::create_dir_all(&step.path).unwrap();
    let why = reject_bad_service(
        &spec,
        "Name: foo\n",
        Some(&snap),
        &step,
        &NullReporter,
        root.path().join("notes.md"),
        &[],
        PKG,
    );
    assert!(why.is_none());
}
