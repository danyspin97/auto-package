//! The single table describing every pipeline step: its prose label, its
//! on-disk directory name and whether it is conditional or retryable.

use crate::event::StepKind;

#[derive(Debug)]
pub struct StepSpec {
    pub kind: StepKind,
    /// Prose shown to users (TUI, text logs).
    pub label: &'static str,
    /// Name used in the numbered run directory (`NN-<dir>`).
    pub dir: &'static str,
    /// The step can be skipped; a skip is reported rather than silent.
    pub conditional: bool,
    /// The step can run again within one package run.
    pub retryable: bool,
    /// A start of this step does not clear the TUI's "build hit ENOSPC" flag:
    /// these are the failure analysis and agent turns that follow that build.
    pub keeps_disk_flag: bool,
}

/// One entry per `StepKind`, in the order of the enum's discriminants, which
/// is also the canonical pipeline order. `StepKind::spec` indexes by it.
pub static STEPS: &[StepSpec] = &[
    StepSpec {
        kind: StepKind::Checkout,
        label: "Checking out package",
        dir: "checkout",
        conditional: false,
        retryable: false,
        keeps_disk_flag: false,
    },
    StepSpec {
        kind: StepKind::OscService,
        label: "Running OBS source service",
        dir: "service",
        conditional: true,
        retryable: false,
        keeps_disk_flag: false,
    },
    StepSpec {
        kind: StepKind::GenerateSpec,
        label: "Generating spec",
        dir: "spec",
        conditional: true,
        retryable: false,
        keeps_disk_flag: false,
    },
    StepSpec {
        kind: StepKind::FetchSdist,
        label: "Fetching sdist",
        dir: "sdist",
        conditional: true,
        retryable: false,
        keeps_disk_flag: false,
    },
    StepSpec {
        kind: StepKind::ZigVendor,
        label: "Refreshing zig vendor archive",
        dir: "zig-vendor",
        conditional: true,
        retryable: false,
        keeps_disk_flag: false,
    },
    StepSpec {
        kind: StepKind::ValidateSpec,
        label: "Validating spec",
        dir: "validate",
        conditional: false,
        retryable: true,
        keeps_disk_flag: false,
    },
    StepSpec {
        kind: StepKind::Commit,
        label: "Committing changes",
        dir: "commit",
        conditional: false,
        retryable: true,
        keeps_disk_flag: false,
    },
    StepSpec {
        kind: StepKind::Build,
        label: "Building on OBS",
        dir: "build",
        conditional: false,
        retryable: true,
        keeps_disk_flag: false,
    },
    StepSpec {
        kind: StepKind::ExtractFailure,
        label: "Extracting failure",
        dir: "extract-failure",
        conditional: true,
        retryable: true,
        keeps_disk_flag: true,
    },
    StepSpec {
        kind: StepKind::AgentTurn,
        label: "Agent turn",
        dir: "agent",
        conditional: false,
        retryable: true,
        keeps_disk_flag: true,
    },
    StepSpec {
        kind: StepKind::PackageNotes,
        label: "Updating package notes",
        dir: "package-notes",
        conditional: true,
        retryable: false,
        keeps_disk_flag: false,
    },
];

impl StepKind {
    pub fn spec(&self) -> &'static StepSpec {
        &STEPS[*self as usize]
    }

    /// Stable snake_case name for machine-readable output.
    pub fn id(&self) -> &'static str {
        match self {
            StepKind::Checkout => "checkout",
            StepKind::OscService => "osc_service",
            StepKind::GenerateSpec => "generate_spec",
            StepKind::FetchSdist => "fetch_sdist",
            StepKind::ZigVendor => "zig_vendor",
            StepKind::ValidateSpec => "validate_spec",
            StepKind::Commit => "commit",
            StepKind::Build => "build",
            StepKind::ExtractFailure => "extract_failure",
            StepKind::AgentTurn => "agent_turn",
            StepKind::PackageNotes => "package_notes",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run::sanitize_name;
    use std::collections::HashSet;

    #[test]
    fn every_kind_has_exactly_one_spec_in_canonical_order() {
        for (i, spec) in STEPS.iter().enumerate() {
            assert_eq!(spec.kind as usize, i, "{} is out of order", spec.label);
            assert!(std::ptr::eq(spec.kind.spec(), spec));
        }
        assert_eq!(STEPS.last().unwrap().kind as usize + 1, STEPS.len());
    }

    #[test]
    fn dirs_and_ids_are_unique_and_dirs_are_sanitized() {
        let mut dirs = HashSet::new();
        let mut ids = HashSet::new();
        for spec in STEPS {
            assert_eq!(sanitize_name(spec.dir), spec.dir);
            assert!(dirs.insert(spec.dir), "duplicate dir {}", spec.dir);
            assert!(ids.insert(spec.kind.id()), "duplicate id {}", spec.dir);
        }
    }
}
