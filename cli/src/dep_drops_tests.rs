
use super::*;

/// Keeps the warnings a drop reports.
#[derive(Default)]
struct Warnings(Mutex<Vec<String>>);

impl Reporter for Warnings {
    fn report(&self, event: &Event) {
        if let Event::Warning { message, .. } = event {
            self.0.lock().unwrap().push(message.clone());
        }
    }
}

/// A dependency dropped for a concrete reason says so in a warning, and the
/// message that later gives up on a package that needs it repeats that
/// reason instead of pointing at output that never gave one.
#[test]
fn a_dropped_dependency_is_reported_and_explained_when_giving_up() {
    let reporter = Warnings::default();
    let drops = DepDrops::default();
    drops.dropped(
        &reporter,
        "vllm-flash-attn",
        "it is unresolvable: no PyPI release and no usable public source repository",
    );
    let warnings = reporter.0.lock().unwrap().clone();
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("vllm-flash-attn"), "{warnings:?}");
    assert!(warnings[0].contains("no PyPI release"), "{warnings:?}");

    let why = drops.explain(&["python-vllm_flash_attn".to_string()]);
    assert!(why.contains("python-vllm_flash_attn"), "{why}");
    assert!(why.contains("no PyPI release"), "{why}");
}

/// The reason a dependency was dropped is the first one; seeing it dropped
/// again as "already attempted" quotes it rather than replacing it, and the
/// outcome of a build replaces a reason because it is newer evidence.
#[test]
fn the_first_reason_is_kept_and_a_build_outcome_replaces_it() {
    let reporter = Warnings::default();
    let drops = DepDrops::default();
    drops.dropped(&reporter, "foo", "the distro already provides it");
    drops.dropped(&reporter, "foo", "it was already attempted this run");
    assert_eq!(
        drops.reason("foo").as_deref(),
        Some("the distro already provides it")
    );
    // Every drop is reported, even a repeat.
    assert_eq!(reporter.0.lock().unwrap().len(), 2);

    drops.note_outcome("python-foo", "its build failed (boom)");
    assert_eq!(
        drops.reason("foo").as_deref(),
        Some("its build failed (boom)")
    );
}

#[test]
fn a_dependency_with_nothing_recorded_is_still_named() {
    let drops = DepDrops::default();
    let why = drops.explain(&["mystery".to_string(), "other".to_string()]);
    assert!(why.contains("mystery") && why.contains("other"), "{why}");
    assert!(why.contains("nothing was recorded"), "{why}");
    assert_eq!(why.lines().count(), 2);
}

/// The vllm run's three given-up packages: the message names each with
/// the reason its dependency is out of reach, and says nothing about
/// "failed attempts" that never happened.
#[test]
fn the_give_up_message_states_each_packages_blocking_reason() {
    let reporter = Warnings::default();
    let drops = DepDrops::default();
    drops.dropped(
        &reporter,
        "fastapi-cloud-cli",
        "it is already in home:dspinella:open-webui as python-fastapi-cloud-cli with \
             status \"succeeded\", which counts as resolvable",
    );
    drops.dropped(
        &reporter,
        "cuda-bindings",
        "it is already in home:dspinella:open-webui as python-cuda-bindings with status \
             \"succeeded\", which counts as resolvable, but python-cuda-bindings is built \
             in home:dspinella:open-webui, but its spec there is an unfinished skeleton",
    );
    drops.dropped(
        &reporter,
        "nvidia-cutlass-dsl-libs-base",
        "python-nvidia-cutlass-dsl-libs-base is excluded for openSUSE_Tumbleweed/i586",
    );
    let mut give_ups = std::collections::BTreeMap::new();
    for (pkg, dep) in [
        ("python-fastapi-cli", "fastapi-cloud-cli"),
        ("python-humming-kernels", "cuda-bindings"),
        (
            "python-nvidia-cutlass-dsl-libs-cu12",
            "nvidia-cutlass-dsl-libs-base",
        ),
    ] {
        let deps = [dep.to_string()];
        give_ups.insert(
            pkg.to_string(),
            format!(
                "it needs {dep} and none of them can be queued again:\n{}",
                drops.explain(&deps)
            ),
        );
    }
    let msg = build_incomplete_message(&give_ups);
    assert!(
        msg.starts_with("build incomplete — gave up on 3 package(s)"),
        "{msg}"
    );
    assert!(!msg.contains("failed attempt"), "{msg}");
    for needle in [
        "- python-fastapi-cli: it needs fastapi-cloud-cli",
        "- python-humming-kernels: it needs cuda-bindings",
        "- python-nvidia-cutlass-dsl-libs-cu12: it needs nvidia-cutlass-dsl-libs-base",
        "status \"succeeded\", which counts as resolvable",
        "unfinished skeleton",
        "is excluded for openSUSE_Tumbleweed/i586",
    ] {
        assert!(msg.contains(needle), "missing {needle:?} in:\n{msg}");
    }
    // A multi-line reason stays under its package.
    let blocks: Vec<&str> = msg.split("\n  - ").skip(1).collect();
    assert_eq!(blocks.len(), 3, "{msg}");
    assert!(blocks[0].contains("\n    - fastapi-cloud-cli:"), "{msg}");
}

#[test]
fn a_failure_reason_is_reported_as_given() {
    let give_ups = std::collections::BTreeMap::from([(
        "python-foo".to_string(),
        "it failed 10 times in a row (last: spec invalid)".to_string(),
    )]);
    let msg = build_incomplete_message(&give_ups);
    assert!(
        msg.ends_with("- python-foo: it failed 10 times in a row (last: spec invalid)"),
        "{msg}"
    );
}
