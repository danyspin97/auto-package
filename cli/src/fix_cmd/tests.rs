use ap_build::bugfix::FixEnd;
use clap::Parser;

use super::plan::*;
use super::*;

fn fix(extra: &[&str]) -> FixArgs {
    let mut argv = vec!["auto-package", "fix", "--project", "openSUSE:Factory"];
    argv.extend_from_slice(extra);
    match crate::Cli::try_parse_from(argv).map(|c| c.command) {
        Ok(crate::Commands::Fix(a)) => a,
        Ok(_) => panic!("not fix"),
        Err(e) => panic!("{e}"),
    }
}

fn parse_error(extra: &[&str]) -> bool {
    let mut argv = vec!["auto-package", "fix"];
    argv.extend_from_slice(extra);
    crate::Cli::try_parse_from(argv).is_err()
}

fn end(verdict: &str) -> FixEnd {
    FixEnd {
        nonce: "n".into(),
        verdict: verdict.into(),
        patch: None,
        applied: None,
        reason: format!("why {verdict}"),
    }
}

#[test]
fn the_arguments_parse_with_a_project_and_a_package() {
    let a = fix(&[
        "--only",
        "curl",
        "--bug",
        "bsc#1234567,CVE-2025-1",
        "--cve",
        "CVE-2025-1234",
        "--commit-url",
        "https://github.com/curl/curl/commit/abc1234",
        "--commit-url",
        "fix.patch",
        "--description",
        "it crashes",
    ]);
    assert_eq!(a.only, ["curl"]);
    assert_eq!(a.bug, ["bsc#1234567", "CVE-2025-1"]);
    assert_eq!(a.commit_url.len(), 2);
    assert!(!a.submit && !a.dry_run && !a.no_agent);
    // A project and a package are required.
    assert!(parse_error(&["--only", "curl"]));
    assert!(parse_error(&["--project", "P"]));
}

#[test]
fn a_fix_with_nothing_to_fix_or_two_packages_or_a_maintenance_project_is_refused() {
    let mut summary = None;
    let e = run(fix(&["--only", "curl"]), &mut summary).unwrap_err();
    assert!(e.to_string().contains("say what to fix"), "{e}");
    let e = run(
        fix(&["--only", "a,b", "--bug", "bsc#1234567"]),
        &mut summary,
    )
    .unwrap_err();
    assert!(e.to_string().contains("exactly one package"), "{e}");
    let mut a = fix(&["--only", "curl", "--bug", "bsc#1234567"]);
    a.project = Some("SUSE:SLE-15-SP7:Update".into());
    let e = run(a, &mut summary).unwrap_err().to_string();
    assert!(
        e.contains("maintenance backport")
            && e.contains(
                "auto-package update --project SUSE:SLE-15-SP7:Update --only curl --maintenance"
            ),
        "{e}"
    );
}

#[test]
fn which_projects_are_maintenance_projects() {
    for p in [
        "SUSE:SLE-15-SP7:Update",
        "SUSE:SLE-15-SP7:GA",
        "openSUSE:Leap:15.6:Update",
        "openSUSE:Leap:16.0",
        "openSUSE:Backports:SLE-15-SP6:Update",
    ] {
        assert!(is_maintenance_project(p), "{p}");
    }
    for p in [
        "openSUSE:Factory",
        "devel:languages:python",
        "home:me:branches:X",
    ] {
        assert!(!is_maintenance_project(p), "{p}");
    }
}

#[test]
fn the_update_arguments_carry_the_fix_and_keep_every_other_default() {
    let a = fix(&[
        "--only",
        "curl",
        "--bug",
        "bsc#1234567",
        "--submit",
        "--budget-usd",
        "3",
        "--max-attempts",
        "4",
        "--no-agent",
        "--follow-devel",
    ]);
    let u = update_args(&a, "home:u:branches:openSUSE:Factory".into(), None);
    let plain = match crate::Cli::try_parse_from([
        "auto-package",
        "update",
        "--project",
        "openSUSE:Factory",
    ])
    .map(|c| c.command)
    {
        Ok(crate::Commands::Update(p)) => p,
        _ => panic!("update parses"),
    };
    assert_eq!(u.source, "openSUSE:Factory");
    assert_eq!(u.only, ["curl"]);
    assert_eq!(
        u.branch_project.as_deref(),
        Some("home:u:branches:openSUSE:Factory")
    );
    assert!(u.submit && u.no_agent && u.follow_devel);
    assert_eq!((u.budget_usd, u.max_attempts), (3.0, 4));
    // Never in place, and nothing a plain update sets besides these.
    assert!(!u.in_place && !u.improve && !u.update_deps && !u.regen && !u.local);
    assert_eq!(
        (
            u.jobs,
            u.min_free_gb,
            u.arch_wait_minutes,
            u.log.as_str(),
            u.arch.as_str()
        ),
        (
            plain.jobs,
            plain.min_free_gb,
            plain.arch_wait_minutes,
            plain.log.as_str(),
            plain.arch.as_str()
        )
    );
    assert!(u.exclude.is_empty() && u.revision.is_none() && u.to_version.is_none());
    assert!(u.maintenance.is_none() && u.backport_job.is_none() && u.fix_task.is_none());
    assert!(!u.dry_run);
    // A dry run asks no model.
    let d = update_args(
        &fix(&["--only", "curl", "--bug", "bsc#1234567", "--dry-run"]),
        "b".into(),
        None,
    );
    assert!(d.dry_run && d.no_agent);
}

#[test]
fn the_run_limits_reach_the_update_arguments() {
    let a = fix(&[
        "--only",
        "curl",
        "--max-total-usd",
        "3",
        "--max-packages",
        "0",
    ]);
    let u = update_args(&a, "home:u:branches:openSUSE:Factory".into(), None);
    assert_eq!((u.limits.max_total_usd, u.limits.max_packages), (3.0, 0));
}

#[test]
fn the_exit_code_comes_from_the_verdict_and_the_run() {
    assert_eq!(ending(true, Some(&end("fixed"))), Ending::Fixed);
    // Fixed but the run failed (a build, a declined request): the run's own error.
    assert_eq!(ending(false, Some(&end("fixed"))), Ending::Failed);
    for v in ["already_fixed", "not_affected", "no_fix"] {
        assert_eq!(
            ending(false, Some(&end(v))),
            Ending::Exit(3, format!("why {v}")),
            "{v}"
        );
    }
    for v in ["needs_maintainer", "does_not_apply"] {
        assert_eq!(
            ending(false, Some(&end(v))),
            Ending::Exit(4, format!("why {v}")),
            "{v}"
        );
    }
    // A record of another run reads as none; so does an interrupted or exhausted stage.
    assert_eq!(ending(false, None), Ending::Failed);
    assert_eq!(ending(false, Some(&end("exhausted"))), Ending::Failed);
    assert_eq!(ending(true, Some(&end("interrupted"))), Ending::Failed);
}
