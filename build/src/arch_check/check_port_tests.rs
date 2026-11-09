//! The port checks and the gate's reads of baselines, against the fake OBS.

use std::path::Path;

use super::check_fake::*;
use super::*;

mod ports_check {
    use super::*;

    const PORT: &str = "openSUSE:Factory:zSystems";
    const REPO: &str = "openSUSE_Factory_zSystems";

    /// The branch with the zSystems port added, as `update --ports` leaves it.
    fn with_port(fake: &FakeObs) {
        fake.metas.borrow_mut().insert(
            BRANCH.into(),
            meta(BRANCH, &[("standard", &["x86_64"]), (REPO, &["s390x"])]),
        );
    }

    fn port_result(fake: &FakeObs, code: &str, details: Option<&str>) {
        let mut e = entry("p", REPO, "s390x", code);
        e.details = details.map(str::to_string);
        set_results(fake, BRANCH, vec![e]);
    }

    fn check(fake: &FakeObs) -> CheckOutcome {
        let dir = tempfile::tempdir().unwrap();
        let say = |_: String| {};
        let mut req = request(dir.path(), &say, false, false);
        req.include_ports = true;
        check_other_arches(fake, &req).unwrap()
    }

    #[test]
    fn a_port_a_run_without_ports_is_not_checked_or_waited_for() {
        let fake = fixture();
        with_port(&fake);
        port_result(&fake, "failed", None);
        set_results(&fake, PORT, vec![entry("p", "standard", "s390x", "failed")]);
        let dir = tempfile::tempdir().unwrap();
        let say = |_: String| {};
        let req = request(dir.path(), &say, false, false);
        let out = check_other_arches(&fake, &req).unwrap();
        assert!(out.rows.is_empty(), "{:?}", out.rows);
        assert!(out.covered);
        assert!(
            !fake
                .events
                .borrow()
                .iter()
                .any(|e| e.starts_with("results:")),
            "{:?}",
            fake.events.borrow()
        );
    }

    #[test]
    fn a_port_the_check_leaves_out_does_not_uncover_the_branch() {
        let fake = fixture();
        with_port(&fake);
        let dir = tempfile::tempdir().unwrap();
        let say = |_: String| {};
        let only = vec!["x86_64".to_string()];
        let mut req = request(dir.path(), &say, false, false);
        req.wanted = Some(only.as_slice());
        let out = check_other_arches(&fake, &req).unwrap();
        assert!(out.rows.is_empty());
        assert!(out.covered);
    }

    #[test]
    fn a_port_failure_factory_has_too_does_not_block() {
        let fake = fixture();
        with_port(&fake);
        port_result(&fake, "failed", None);
        set_results(&fake, PORT, vec![entry("p", "standard", "s390x", "failed")]);
        let out = check(&fake);
        let row = &out.rows[0];
        assert_eq!(row.repo, REPO);
        assert!(
            row.baseline == BaselineState::Fails && !row.blocks_submit(),
            "{row:?}"
        );
        assert!(
            fake.events
                .borrow()
                .iter()
                .any(|e| e.starts_with(&format!("results:{PORT}:")))
        );
    }

    #[test]
    fn a_port_failure_factory_builds_blocks() {
        let fake = fixture();
        with_port(&fake);
        port_result(&fake, "failed", None);
        set_results(
            &fake,
            PORT,
            vec![entry("p", "standard", "s390x", "succeeded")],
        );
        let row = check(&fake).rows.remove(0);
        assert!(row.blocks_submit(), "{row:?}");
        assert_eq!(row.state, ArchState::Failed { detail: None });
    }

    #[test]
    fn a_port_failure_without_a_factory_result_is_a_note_as_no_baseline() {
        let fake = fixture();
        with_port(&fake);
        port_result(&fake, "failed", None);
        set_results(&fake, PORT, vec![]);
        let row = check(&fake).rows.remove(0);
        assert_eq!(row.state, ArchState::NoBaseline { detail: None });
        assert_eq!(row.baseline, BaselineState::NoResult);
        assert!(!row.blocks_submit());
    }

    #[test]
    fn an_unresolvable_port_is_not_testable_and_factory_is_not_read() {
        let fake = fixture();
        with_port(&fake);
        port_result(
            &fake,
            "unresolvable",
            Some("nothing provides libwlroots-0_20 needed by river"),
        );
        let row = check(&fake).rows.remove(0);
        assert_eq!(
            row.state,
            ArchState::NotTestable {
                detail: Some("nothing provides libwlroots-0_20".to_string())
            }
        );
        assert!(!row.blocks_submit());
        assert!(
            !fake
                .events
                .borrow()
                .iter()
                .any(|e| e.starts_with("results:openSUSE:Factory:zSystems"))
        );
    }

    #[test]
    fn a_port_still_building_is_not_final_and_does_not_block() {
        let fake = fixture();
        with_port(&fake);
        port_result(&fake, "scheduled", None);
        let out = check(&fake);
        assert_eq!(
            out.rows[0].state,
            ArchState::NotFinal("scheduled".to_string())
        );
        assert!(!out.rows[0].blocks_submit());
        assert!(
            out.notes.iter().any(|n| n.contains("not final")),
            "{:?}",
            out.notes
        );
    }
}

mod gate_ports {
    use super::*;

    const PORT: &str = "openSUSE:Factory:zSystems";
    const REPO: &str = "openSUSE_Factory_zSystems";

    fn gate(fake: &FakeObs) -> GateVerdict {
        let rows = gate_inputs(fake, BRANCH, ORIGIN, "p").unwrap();
        gate_verdict("p", ("standard", "x86_64"), &rows, false, false)
    }

    fn branch_with_port(fake: &FakeObs, code: &str) {
        fake.metas.borrow_mut().insert(
            BRANCH.into(),
            meta(BRANCH, &[("standard", &["x86_64"]), (REPO, &["s390x"])]),
        );
        set_results(
            fake,
            BRANCH,
            vec![
                entry("p", "standard", "x86_64", "succeeded"),
                entry("p", REPO, "s390x", code),
            ],
        );
    }

    #[test]
    fn a_port_failure_factory_has_too_passes_with_a_note() {
        let fake = fixture();
        branch_with_port(&fake, "failed");
        set_results(&fake, PORT, vec![entry("p", "standard", "s390x", "failed")]);
        let GateVerdict::Pass { notes, .. } = gate(&fake) else {
            panic!("a failure Factory has too does not block");
        };
        assert!(
            notes[0].contains(&format!("also fails in {PORT} on {REPO}/s390x")),
            "{notes:?}"
        );
    }

    #[test]
    fn a_port_failure_without_a_factory_result_passes_with_a_note() {
        let fake = fixture();
        branch_with_port(&fake, "failed");
        set_results(&fake, PORT, vec![]);
        let GateVerdict::Pass { notes, .. } = gate(&fake) else {
            panic!("a port failure Factory has no result for is a note");
        };
        assert!(
            notes.iter().any(|n| n.contains("has no result")),
            "{notes:?}"
        );
    }

    #[test]
    fn an_untestable_or_unfinished_port_does_not_refuse() {
        let fake = fixture();
        branch_with_port(&fake, "unresolvable");
        set_results(&fake, PORT, vec![]);
        assert!(matches!(gate(&fake), GateVerdict::Pass { .. }));
        branch_with_port(&fake, "scheduled");
        assert!(matches!(gate(&fake), GateVerdict::Pass { .. }));
    }
}

mod ports_edit {
    use super::*;
    use crate::osc::parse_project_info;

    const PORT: &str = "openSUSE_Factory_zSystems";

    fn zsystems_request<'a>(
        log_dir: &'a Path,
        say: &'a dyn Fn(String),
        read_only: bool,
    ) -> CheckRequest<'a> {
        let mut req = request(log_dir, say, false, read_only);
        req.ports = ports_wanted(Some(&["s390x".to_string()]));
        req.include_ports = true;
        req
    }

    fn index_of(events: &[String], prefix: &str) -> usize {
        events
            .iter()
            .position(|e| e.starts_with(prefix))
            .unwrap_or_else(|| panic!("no {prefix} in {events:?}"))
    }

    #[test]
    fn a_port_is_added_once_enabled_for_the_package_and_then_read() {
        let fake = fixture();
        let dir = tempfile::tempdir().unwrap();
        let say = |m: String| fake.log(format!("say:{m}"));
        set_results(&fake, BRANCH, vec![entry("p", PORT, "s390x", "succeeded")]);
        let out = check_other_arches(&fake, &zsystems_request(dir.path(), &say, false)).unwrap();
        assert_eq!(fake.writes(), [format!("write:{BRANCH}")]);
        let branch = parse_project_info(&fake.metas.borrow()[BRANCH]);
        assert_eq!(repo_arches(&branch, PORT), ["s390x"]);
        assert!(fake.metas.borrow()[BRANCH].contains(&format!("<disable repository=\"{PORT}\"/>")));
        let events = fake.events.borrow().clone();
        let write = index_of(&events, "write:");
        let enable = index_of(&events, &format!("enable:{BRANCH}:p:{PORT}"));
        let read = index_of(&events, &format!("results:{BRANCH}:p"));
        assert!(write < enable && enable < read, "{events:?}");
        assert!(
            events
                .iter()
                .any(|e| e.starts_with("say:adding repository")),
            "{events:?}"
        );
        assert_eq!(out.rows.len(), 1);
        assert_eq!(out.rows[0].state, ArchState::Succeeded);
    }

    #[test]
    fn a_second_run_writes_nothing_more() {
        let fake = fixture();
        let dir = tempfile::tempdir().unwrap();
        let say = |_: String| {};
        set_results(&fake, BRANCH, vec![entry("p", PORT, "s390x", "succeeded")]);
        check_other_arches(&fake, &zsystems_request(dir.path(), &say, false)).unwrap();
        check_other_arches(&fake, &zsystems_request(dir.path(), &say, false)).unwrap();
        assert_eq!(fake.writes().len(), 1, "{:?}", fake.events.borrow());
    }

    #[test]
    fn a_read_only_check_neither_adds_nor_enables_a_port() {
        let fake = fixture();
        let dir = tempfile::tempdir().unwrap();
        let say = |_: String| {};
        let out = check_other_arches(&fake, &zsystems_request(dir.path(), &say, true)).unwrap();
        assert!(fake.writes().is_empty());
        assert!(
            !fake
                .events
                .borrow()
                .iter()
                .any(|e| e.starts_with("enable:"))
        );
        assert!(
            out.notes.iter().any(|n| n.contains("only read")),
            "{:?}",
            out.notes
        );
    }

    #[test]
    fn a_port_is_refused_for_a_project_that_is_not_home_before_any_read() {
        let fake = fixture();
        let dir = tempfile::tempdir().unwrap();
        let say = |_: String| {};
        let mut req = zsystems_request(dir.path(), &say, false);
        req.branch = "devel:languages:rust";
        assert!(check_other_arches(&fake, &req).is_err());
        assert!(
            fake.events.borrow().is_empty(),
            "{:?}",
            fake.events.borrow()
        );
    }
}
