use super::network_fetch::network_fetch_failure;

const ZIG_LOG: &str = "\
[   23s] + /usr/bin/zig build --verbose -Dtarget=native
[   33s] /home/abuild/rpmbuild/BUILD/river-0.5.0-build/river-0.5.0/build.zig.zon:23:20: error: unable to discover remote git server capabilities: NameServerFailure
[   33s]             .url = \"git+https://codeberg.org/ifreund/zig-wlroots#c183af18\",
";

#[test]
fn a_zig_git_dependency_that_cannot_be_fetched_is_a_network_fetch() {
    let line = network_fetch_failure(ZIG_LOG).expect("network fetch");
    assert!(line.contains("NameServerFailure"), "{line}");
}

#[test]
fn a_cargo_download_that_cannot_resolve_its_host_is_a_network_fetch() {
    let log = "error: failed to get `serde` as a dependency\n\
               Caused by:\n  failed to download `serde v1.0.0`\n\
               Caused by:\n  [6] Could not resolve hostname (Could not resolve host: index.crates.io)\n";
    assert!(network_fetch_failure(log).is_some());
}

#[test]
fn a_name_error_without_a_fetch_is_not_a_network_fetch() {
    let log =
        "test dns::lookup ... FAILED\nthread panicked: Could not resolve host: example.test\n";
    assert_eq!(network_fetch_failure(log), None);
}

#[test]
fn a_compile_error_is_not_a_network_fetch() {
    let log = "src/main.c:12:3: error: implicit declaration of function 'foo'\n";
    assert_eq!(network_fetch_failure(log), None);
}

#[test]
fn the_evidence_line_is_short() {
    let long = format!("zig fetch {}", "x".repeat(900));
    let log = format!("{long} NameServerFailure\n");
    let line = network_fetch_failure(&log).expect("network fetch");
    assert!(line.chars().count() <= 300);
}
