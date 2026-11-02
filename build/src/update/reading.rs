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
        run_with(&list, cat, incident, go_on)
    }

    /// [`run`] with osc's listing answers as they are: the file list, or the
    /// text of osc's error.
    fn run_with(
        list: &[Result<Vec<String>, String>],
        cat: &[Option<&str>],
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
            &|_| cat[nth(&r.cats, cat.len())].map(str::to_string),
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
    fn a_broken_link_is_not_retried_and_names_its_target() {
        let (out, r) = run_with(
            &[Err(BROKEN_LINK.to_string()), Ok(files(&["stacktile.spec"]))],
            &[Some("Name: stacktile")],
            None,
            true,
        );
        let Err(ReadError::Failed(why)) = out else {
            panic!("expected a failed read, got {out:?}");
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
            &[None],
            None,
            true,
        );
        let Err(ReadError::Failed(why)) = out else {
            panic!("expected a failed read, got {out:?}");
        };
        assert!(why.contains("does not exist on OBS"), "{why}");
        assert_eq!(r.lists.get(), 1);
    }

    #[test]
    fn a_bare_400_or_404_or_does_not_exist_is_retried() {
        for said in [
            "HTTP Error 404: Not Found",
            "HTTP Error 400: Bad Request",
            "osc: package 'pkg' does not exist",
        ] {
            let (out, r) = run_with(&[Err(said.to_string())], &[None], None, true);
            assert!(matches!(out, Err(ReadError::Failed(_))), "{said}: {out:?}");
            assert_eq!(r.lists.get(), 3, "{said}");
        }
    }

    #[test]
    fn a_timed_out_listing_is_still_retried() {
        let (out, r) = run_with(
            &[
                Err("Connection timed out".to_string()),
                Err("HTTP Error 503: Service Unavailable".to_string()),
                Ok(files(&["pkg.spec"])),
            ],
            &[Some("Name: pkg")],
            None,
            true,
        );
        assert_eq!(out.as_deref(), Ok("Name: pkg"));
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
    }
}
