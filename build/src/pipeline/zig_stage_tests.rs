//! The zig vendor stage of an update, with a checkout made in a temp dir. The
//! refusal under test is the host allowlist, which refuses before any request,
//! so no test here reaches the network.

use super::*;
use crate::pipeline::hygiene_tests::config;
use crate::pipeline::zig_stage::zig_vendor_stage;
use crate::workspace::Workspace;

const SPEC: &str = "Name:           pkg\n\
    Version:        1.0.0\n\
    Release:        0\n\
    Summary:        test\n\
    License:        MIT\n\
    Source0:        https://example.org/%{name}-%{version}.tar.gz\n\
    Source2:        vendor.tar.zst\n\
    BuildRequires:  zig >= 0.16.0\n\
    \n\
    %description\n\
    test\n\
    \n\
    %prep\n\
    %autosetup -n %{name}-%{version} -a2\n\
    \n\
    %build\n\
    %zig_build --global-cache-dir vendor/\n\
    \n\
    %install\n\
    %zig_install\n";

/// A gzipped tar of `files` under `top/`.
fn tar_gz(top: &str, files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut ar = tar::Builder::new(flate2::write::GzEncoder::new(
        Vec::new(),
        flate2::Compression::fast(),
    ));
    for (path, content) in files {
        let mut h = tar::Header::new_gnu();
        h.set_size(content.len() as u64);
        h.set_mode(0o644);
        h.set_cksum();
        ar.append_data(&mut h, format!("{top}/{path}"), *content)
            .unwrap();
    }
    ar.into_inner().unwrap().finish().unwrap()
}

/// A new Source0 whose manifest depends on a package at `url`.
fn source0(url: &str) -> Vec<u8> {
    let zon = format!(
        ".{{ .name = .pkg, .version = \"1.0.0\", .fingerprint = 0x3610a686c4bba334, \
         .dependencies = .{{ .other = .{{ .url = \"{url}\", \
         .hash = \"other-0.1.0-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA\" }}, }}, }}"
    );
    tar_gz("pkg-1.0.0", &[("build.zig.zon", zon.as_bytes())])
}

/// Run the zig vendor stage of an update of `spec` whose new Source0 is `src0`.
fn run_stage(spec: &str, src0: &[u8], update: bool) -> (tempfile::TempDir, PipelineResult) {
    let root = tempfile::tempdir().unwrap();
    let work = root.path().join("work");
    let state_root = work.join("state");
    let mut config = config("pkg", &state_root);
    config.work_dir = work.clone();
    config.project = "proj".into();
    config.update = update;
    config.version = "1.0.0".into();
    let ws = Workspace::open(&state_root, "pkg").unwrap();
    let checkout = work.join("proj").join("pkg");
    std::fs::create_dir_all(&checkout).unwrap();
    let spec_path = checkout.join("pkg.spec");
    std::fs::write(&spec_path, spec).unwrap();
    std::fs::write(checkout.join("pkg-1.0.0.tar.gz"), src0).unwrap();
    std::fs::write(checkout.join("vendor.tar.zst"), b"old").unwrap();
    let run = ap_core::run::RunDir::allocate(&work).unwrap();
    let pkg_run = run.pkg("pkg");
    let ctx = RunCtx {
        config: &config,
        pkg_run: &pkg_run,
        reporter: &ap_core::sink::NULL_REPORTER,
        ws,
        rpm_name: "pkg".into(),
        obs_name: "pkg".into(),
        checkout_dir: checkout,
        spec_path,
        signals: Default::default(),
    };
    let mut st = RunState {
        step_attempts: Default::default(),
        sdist_top_level: String::new(),
        from_repo_archive: false,
        attempts: 0,
        spent_usd: 0.0,
        session_id: None,
        last_log_digest: None,
        identical_logs: 0,
        same_shape_failures: 0,
        last_failure_shape: None,
        relaxed_from_spec: None,
        post_review_done: false,
        run_start_spec: None,
        post_review_revert: None,
        rpmlint_shown: Default::default(),
        update: None,
        carried: None,
        build_rounds: Default::default(),
    };
    let result = match zig_vendor_stage(&ctx, &mut st) {
        Ok(()) => PipelineResult::Failed {
            rpm_name: "not refused".into(),
            reason: String::new(),
            kind: FailureKind::Other,
            remedy: None,
            note: None,
        },
        Err(result) => result,
    };
    (root, result)
}

fn reason(result: PipelineResult) -> String {
    match result {
        PipelineResult::Failed { reason, .. } => reason,
        other => panic!("expected a failure, got {other:?}"),
    }
}

#[test]
fn a_package_that_is_not_updated_is_left_alone() {
    let src0 = source0("https://evil.example.net/other.tar.gz");
    let (_root, result) = run_stage(SPEC, &src0, false);
    assert!(
        matches!(result, PipelineResult::Failed { ref rpm_name, .. } if rpm_name == "not refused"),
        "{result:?}"
    );
}

#[test]
fn a_spec_without_a_zig_vendor_archive_is_left_alone() {
    let src0 = source0("https://evil.example.net/other.tar.gz");
    let no_zig = SPEC.replace("BuildRequires:  zig >= 0.16.0\n", "BuildRequires:  cmake\n");
    let (_root, result) = run_stage(&no_zig, &src0, true);
    assert!(
        matches!(result, PipelineResult::Failed { ref rpm_name, .. } if rpm_name == "not refused"),
        "{result:?}"
    );
}

#[test]
fn a_dependency_off_the_allowlist_ends_the_package_with_its_reason() {
    let src0 = source0("https://evil.example.net/other.tar.gz");
    let (root, result) = run_stage(SPEC, &src0, true);
    let why = reason(result);
    assert!(why.contains("cannot be refreshed"), "{why}");
    assert!(why.contains("evil.example.net"), "{why}");
    // The old archive is what the checkout keeps.
    let kept = std::fs::read(root.path().join("work/proj/pkg/vendor.tar.zst")).unwrap();
    assert_eq!(kept, b"old");
}

#[test]
fn an_xz_archive_is_refreshed_like_the_others_and_stops_at_the_allowlist() {
    let src0 = source0("https://evil.example.net/other.tar.gz");
    let xz = SPEC.replace("vendor.tar.zst", "vendor.tar.xz");
    let (_root, result) = run_stage(&xz, &src0, true);
    let why = reason(result);
    assert!(why.contains("evil.example.net"), "{why}");
}
