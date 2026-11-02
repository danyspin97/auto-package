//! The port checks and the gate's reads of baselines, against the fake OBS.

use std::path::Path;
use std::time::Duration;

use super::check_fake::*;
use super::*;
use crate::obs_status::ResultEntry;

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
        let req = request(dir.path(), &say, false, false);
        check_other_arches(fake, &req).unwrap()
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
        assert!(row.baseline_fails && !row.blocks_submit(), "{row:?}");
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
    fn a_port_failure_without_a_factory_result_blocks_as_no_baseline() {
        let fake = fixture();
        with_port(&fake);
        port_result(&fake, "failed", None);
        set_results(&fake, PORT, vec![]);
        let row = check(&fake).rows.remove(0);
        assert_eq!(row.state, ArchState::NoBaseline { detail: None });
        assert!(row.blocks_submit());
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
        let GateVerdict::Pass { notes } = gate(&fake) else {
            panic!("a failure Factory has too does not block");
        };
        assert!(
            notes[0].contains(&format!("also fails in {PORT} on {REPO}/s390x")),
            "{notes:?}"
        );
    }

    #[test]
    fn a_port_failure_without_a_factory_result_refuses() {
        let fake = fixture();
        branch_with_port(&fake, "failed");
        set_results(&fake, PORT, vec![]);
        let GateVerdict::Refuse(why) = gate(&fake) else {
            panic!("a port failure Factory does not build blocks");
        };
        assert!(why.contains("failed (no Factory baseline)"), "{why}");
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
