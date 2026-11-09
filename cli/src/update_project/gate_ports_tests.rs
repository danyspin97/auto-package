use super::arch::gate_counts_ports;
use super::*;
use crate::{Cli, Commands};
use clap::Parser;

fn args(extra: &[&str]) -> UpdateProjectArgs {
    let mut argv = vec!["auto-package", "update", "--project", "Base:System"];
    argv.extend_from_slice(extra);
    match Cli::try_parse_from(argv).unwrap().command {
        Commands::Update(a) => a,
        _ => panic!("not update"),
    }
}

#[test]
fn the_gate_counts_ports_only_when_the_run_checks_them() {
    assert!(!gate_counts_ports(&args(&[])));
    assert!(gate_counts_ports(&args(&["--ports"])));
    assert!(gate_counts_ports(&args(&["--arches", "s390x"])));
    assert!(!gate_counts_ports(&args(&["--arches", "x86_64"])));
    assert!(!gate_counts_ports(&args(&["--ports", "--local"])));
}
