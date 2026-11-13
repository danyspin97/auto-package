use super::*;
use crate::pipeline::generate_service::osc_service_stage;
use crate::pipeline::run::SpecInfo;
use crate::pipeline::sdist_stage_tests::with_checkout_as;

/// Keeps the kinds of the steps the stage skips.
#[derive(Default)]
struct Skips(std::sync::Mutex<Vec<StepKind>>);

impl Reporter for Skips {
    fn report(&self, e: &Event) {
        if let Event::StepSkipped { kind, .. } = e {
            self.0.lock().unwrap().push(*kind);
        }
    }
}

fn python_package() -> SpecInfo {
    SpecInfo {
        need_spec: false,
        is_maturin: false,
        is_rust: false,
        carried_from: None,
        new_package: false,
    }
}

#[test]
fn a_python_package_without_a_service_skips_nothing_in_the_service_stage() {
    let skips = Skips::default();
    with_checkout_as(&skips, "1.0", None, "", "", |ctx, st| {
        osc_service_stage(ctx, st, &python_package()).unwrap_or_else(|_| panic!("the stage fails"));
    });
    assert!(skips.0.lock().unwrap().is_empty());
}

#[test]
fn a_python_package_whose_service_makes_the_source_is_not_skipped_here() {
    // The source pass runs this service and opens the OscService step itself.
    let skips = Skips::default();
    let tar_scm = "<services><service name=\"tar_scm\"/></services>\n";
    with_checkout_as(&skips, "1.0", None, "", tar_scm, |ctx, st| {
        osc_service_stage(ctx, st, &python_package()).unwrap_or_else(|_| panic!("the stage fails"));
    });
    assert!(skips.0.lock().unwrap().is_empty());
}
