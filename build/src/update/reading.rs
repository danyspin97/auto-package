mod reading {
    use super::super::*;
    use std::cell::{Cell, RefCell};
    use std::time::Duration;

    fn files(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    /// Runs `read_spec` with a listing and a cat that answer from `list`
    /// and `cat` (one answer per call, the last one repeating), counting
    /// the calls and recording the waits.
    struct Run {
        lists: Cell<usize>,
        cats: Cell<usize>,
        waits: RefCell<Vec<Duration>>,
    }

    fn run(
        list: &[Option<Vec<String>>],
        cat: &[Option<&str>],
        incident: Option<Result<String, ReadError>>,
        go_on: bool,
    ) -> (Result<String, ReadError>, Run) {
        let list: Vec<Result<Vec<String>, String>> = list
            .iter()
            .map(|f| {
                f.clone()
                    .ok_or_else(|| "osc ls could not be run".to_string())
            })
            .collect();
        let cat: Vec<Result<&str, &str>> = cat
            .iter()
            .map(|c| c.ok_or("osc cat could not be run"))
            .collect();
        run_with(&list, &cat, incident, go_on)
    }

    /// [`run`] with osc's answers as they are: the file list or the text of
    /// osc's error, and the text of a file or the text of osc's error.
    fn run_with(
        list: &[Result<Vec<String>, String>],
        cat: &[Result<&str, &str>],
        incident: Option<Result<String, ReadError>>,
        go_on: bool,
    ) -> (Result<String, ReadError>, Run) {
        let r = Run {
            lists: Cell::new(0),
            cats: Cell::new(0),
            waits: RefCell::new(Vec::new()),
        };
        let nth = |n: &Cell<usize>, len: usize| {
            let i = n.get().min(len - 1);
            n.set(n.get() + 1);
            i
        };
        let out = read_spec(
            "pkg",
            &|| list[nth(&r.lists, list.len())].clone(),
            &|_| {
                cat[nth(&r.cats, cat.len())]
                    .map(str::to_string)
                    .map_err(str::to_string)
            },
            &|| incident.clone(),
            &|d| {
                r.waits.borrow_mut().push(d);
                go_on
            },
        );
        (out, r)
    }

    #[test]
    fn a_package_with_no_spec_is_not_retried() {
        let (out, r) = run(&[Some(files(&["_service", "README"]))], &[None], None, true);
        assert_eq!(out, Err(ReadError::NoSpec));
        assert_eq!(r.lists.get(), 1);
        assert_eq!(r.cats.get(), 0);
        assert!(r.waits.borrow().is_empty());
        // An empty listing without an incident copy is no spec either.
        let (out, r) = run(&[Some(vec![])], &[None], None, true);
        assert_eq!(out, Err(ReadError::NoSpec));
        assert_eq!(r.lists.get(), 1);
    }

    #[test]
    fn an_empty_package_reads_its_newest_incident_copy() {
        let (out, _) = run(&[Some(vec![])], &[None], Some(Ok("Name: x".into())), true);
        assert_eq!(out.as_deref(), Ok("Name: x"));
        let (out, _) = run(&[Some(vec![])], &[None], Some(Err(ReadError::NoSpec)), true);
        assert_eq!(out, Err(ReadError::NoSpec));
    }

    #[test]
    fn a_failed_read_is_retried_with_a_growing_wait() {
        let (out, r) = run(
            &[None, Some(files(&["pkg.spec"]))],
            &[None, Some("Name: pkg")],
            None,
            true,
        );
        assert_eq!(out.as_deref(), Ok("Name: pkg"));
        assert_eq!(r.lists.get(), 3);
        assert_eq!(
            *r.waits.borrow(),
            [Duration::from_secs(5), Duration::from_secs(10)]
        );
    }

    #[test]
    fn giving_up_says_which_step_failed() {
        let (out, r) = run(&[None], &[None], None, true);
        let Err(ReadError::Failed(why)) = out else {
            panic!("expected a failed read");
        };
        assert!(
            why.contains("osc ls") && why.contains("3 attempts"),
            "{why}"
        );
        assert_eq!(r.lists.get(), 3);
        let (out, r) = run(&[Some(files(&["pkg.spec"]))], &[None], None, true);
        let Err(ReadError::Failed(why)) = out else {
            panic!("expected a failed read");
        };
        assert!(why.contains("pkg.spec") && why.contains("osc cat"), "{why}");
        assert_eq!(r.cats.get(), 3);
    }

    #[test]
    fn an_interrupt_stops_the_retries() {
        let (out, r) = run(&[None], &[None], None, false);
        assert_eq!(out, Err(ReadError::Failed("interrupted".into())));
        assert_eq!(r.lists.get(), 1);
        assert_eq!(r.waits.borrow().len(), 1);
    }

    const BROKEN_LINK: &str = "Link \"X11:Wayland/stacktile\" cannot be expanded:\n \
         openSUSE:Factory/stacktile: package 'stacktile' does not exist\n\n\
         Use \"osc repairlink\" to fix merge conflicts.\n";

    #[test]
    fn a_broken_link_is_not_retried_and_goes_to_the_maintainer() {
        let (out, r) = run_with(
            &[Err(BROKEN_LINK.to_string()), Ok(files(&["stacktile.spec"]))],
            &[Ok("Name: stacktile")],
            None,
            true,
        );
        let Err(ReadError::NeedsMaintainer(why)) = out else {
            panic!("expected a read that needs the maintainer, got {out:?}");
        };
        assert!(why.contains("openSUSE:Factory/stacktile"), "{why}");
        assert!(why.contains("relink or unlink"), "{why}");
        assert_eq!(r.lists.get(), 1);
        assert!(r.waits.borrow().is_empty());
    }

    #[test]
    fn a_missing_package_is_not_retried() {
        let (out, r) = run_with(
            &[Err(
                "HTTP Error 404: Not Found\nunknown_package: no such package".to_string(),
            )],
            &[Err("unused")],
            None,
            true,
        );
        let Err(ReadError::Absent(why)) = out else {
            panic!("expected an absent package, got {out:?}");
        };
        assert!(why.contains("does not exist on OBS"), "{why}");
        assert_eq!(r.lists.get(), 1);
    }

    #[test]
    fn what_osc_prints_for_a_missing_package_or_project_is_not_retried() {
        for said in [
            "Server returned an error: HTTP Error 404: Not Found\nPackage not found: home:u:branches:Base:System/foo",
            "Server returned an error: HTTP Error 404: Not Found\nProject not found: home:u:branches:Base:System",
        ] {
            let (out, r) = run_with(&[Err(said.to_string())], &[Err("unused")], None, true);
            let Err(ReadError::Absent(why)) = out else {
                panic!("expected an absent package, got {out:?}");
            };
            assert!(why.contains("does not exist on OBS"), "{said}: {why}");
            assert_eq!(r.lists.get(), 1, "{said}");
            assert!(r.waits.borrow().is_empty(), "{said}");
        }
    }

    #[test]
    fn a_bare_400_or_404_or_does_not_exist_is_retried() {
        for said in [
            "HTTP Error 404: Not Found",
            "HTTP Error 400: Bad Request",
            "osc: package 'pkg' does not exist",
        ] {
            let (out, r) = run_with(&[Err(said.to_string())], &[Err("unused")], None, true);
            assert!(matches!(out, Err(ReadError::Failed(_))), "{said}: {out:?}");
            assert_eq!(r.lists.get(), 3, "{said}");
        }
    }

    #[test]
    fn a_404_for_a_listed_file_is_retried_once_then_refused() {
        let (out, r) = run_with(
            &[Ok(files(&["pkg.spec"]))],
            &[Err("HTTP Error 404: Not Found")],
            None,
            true,
        );
        let Err(ReadError::Failed(why)) = out else {
            panic!("expected a failed read, got {out:?}");
        };
        assert!(why.contains("again after a retry"), "{why}");
        assert_eq!(r.cats.get(), 2);
        assert_eq!(r.waits.borrow().len(), 1);
    }

    #[test]
    fn a_listed_file_that_disappears_is_not_retried() {
        let (out, r) = run_with(
            &[Ok(files(&["pkg.spec"])), Ok(files(&["other.spec"]))],
            &[Err("HTTP Error 404: Not Found")],
            None,
            true,
        );
        let Err(ReadError::Failed(why)) = out else {
            panic!("expected a failed read, got {out:?}");
        };
        assert!(why.contains("no longer listed"), "{why}");
        assert_eq!((r.lists.get(), r.cats.get()), (2, 1));
        assert!(r.waits.borrow().is_empty());
    }

    #[test]
    fn a_bare_400_on_a_listed_file_is_retried() {
        let (out, r) = run_with(
            &[Ok(files(&["pkg.spec"]))],
            &[Err("HTTP Error 400: Bad Request")],
            None,
            true,
        );
        assert!(out.is_err(), "{out:?}");
        assert_eq!(r.cats.get(), 3);
    }

    #[test]
    fn a_timed_out_listing_is_still_retried() {
        let (out, r) = run_with(
            &[
                Err("Connection timed out".to_string()),
                Err("HTTP Error 503: Service Unavailable".to_string()),
                Ok(files(&["pkg.spec"])),
            ],
            &[Ok("Name: pkg")],
            None,
            true,
        );
        assert_eq!(out.as_deref(), Ok("Name: pkg"));
        assert_eq!(r.lists.get(), 3);
    }

    #[test]
    fn a_timed_out_read_of_a_listed_file_is_still_retried() {
        let (out, r) = run_with(
            &[Ok(files(&["pkg.spec"]))],
            &[Err("Connection timed out"), Ok("Name: pkg")],
            None,
            true,
        );
        assert_eq!(out.as_deref(), Ok("Name: pkg"));
        assert_eq!(r.cats.get(), 2);
    }

    #[test]
    fn an_invalid_name_is_not_retried() {
        let (out, r) = run_with(
            &[Err(
                "X11:Wayland/bad name is not a valid OBS name".to_string()
            )],
            &[Err("unused")],
            None,
            true,
        );
        let Err(ReadError::Failed(why)) = out else {
            panic!("expected a failed read, got {out:?}");
        };
        assert!(why.contains("not a valid OBS name"), "{why}");
        assert_eq!(r.lists.get(), 1);
    }

    #[test]
    fn a_refused_credential_is_not_retried_and_says_so() {
        for status in ["401: Unauthorized", "403: Forbidden"] {
            let (out, r) = run_with(
                &[Err(format!("HTTP Error {status}"))],
                &[Err("unused")],
                None,
                true,
            );
            let Err(ReadError::Failed(why)) = out else {
                panic!("expected a failed read, got {out:?}");
            };
            assert!(why.contains("read-protected"), "{why}");
            assert_eq!(r.lists.get(), 1);
            assert!(r.waits.borrow().is_empty());
        }
    }

    #[test]
    fn a_package_osc_says_is_not_there_is_absent_and_a_gateway_error_is_a_failed_read() {
        let (out, r) = run_with(
            &[Err(
                "HTTP Error 404: Not Found\nPackage not found: P/pkg".into()
            )],
            &[Ok("")],
            None,
            true,
        );
        assert!(matches!(out, Err(ReadError::Absent(_))), "{out:?}");
        assert_eq!(r.lists.get(), 1);
        assert!(r.waits.borrow().is_empty());

        let (out, r) = run_with(
            &[Err("HTTP Error 503: Service Unavailable".into())],
            &[Ok("")],
            None,
            true,
        );
        assert!(matches!(out, Err(ReadError::Failed(_))), "{out:?}");
        assert_eq!(r.lists.get(), 3);
    }

    #[test]
    fn the_two_failures_are_different_skip_reasons() {
        assert_eq!(SkipReason::from(ReadError::NoSpec), SkipReason::NoSpec);
        assert_eq!(
            SkipReason::from(ReadError::Failed("x".into())),
            SkipReason::ReadFailed("x".into())
        );
        assert_ne!(
            SkipReason::NoSpec.to_string(),
            SkipReason::ReadFailed("x".into()).to_string()
        );
        // A 404 is a skip of its own, not a failed read that ends the run.
        assert_eq!(
            SkipReason::from(ReadError::Absent("x".into())),
            SkipReason::NotOnObs("x".into())
        );
    }
}
