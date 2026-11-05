//! The `--check-jobs` flag of `update` and `lint`: how it parses.

use super::*;
use clap::Parser;

fn jobs(args: &[&str]) -> Result<Option<usize>, clap::Error> {
    match Cli::try_parse_from(args)?.command {
        Commands::Update(a) | Commands::Lint(a) => Ok(a.check_jobs),
        _ => panic!("not update or lint"),
    }
}

#[test]
fn check_jobs_is_unset_unless_given_and_must_be_at_least_one() {
    let update = ["auto-package", "update", "--project", "home:u"];
    assert_eq!(jobs(&update).unwrap(), None);
    let with = |n: &str| {
        jobs(&[
            "auto-package",
            "lint",
            "--project",
            "home:u",
            "--check-jobs",
            n,
        ])
    };
    assert_eq!(with("3").unwrap(), Some(3));
    assert_eq!(with("1").unwrap(), Some(1));
    assert!(with("0").is_err());
    assert!(with("many").is_err());
}
