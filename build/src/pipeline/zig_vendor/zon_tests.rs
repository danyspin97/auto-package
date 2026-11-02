use super::zon::*;

/// River 0.5.0's `build.zig.zon`, copied from its release tarball.
const RIVER: &str = r#"// SPDX-FileCopyrightText: © 2024 The River Developers
// SPDX-License-Identifier: GPL-3.0-only
.{
    .name = .river,
    // While a river release is in development, this string should contain
    // the version in development with the "-dev" suffix.
    .version = "0.5.0",
    .paths = .{""},
    .dependencies = .{
        .pixman = .{
            .url = "https://codeberg.org/ifreund/zig-pixman/archive/v0.3.0.tar.gz",
            .hash = "pixman-0.3.0-LClMnz2VAAAs7QSCGwLimV5VUYx0JFnX5xWU6HwtMuDX",
        },
        .wayland = .{
            .url = "https://codeberg.org/ifreund/zig-wayland/archive/v0.6.0.tar.gz",
            .hash = "wayland-0.6.0-lQa1kqz8AQADQmdNJsNhLoNHcnEGEUjrOaPV-dtEnEmX",
        },
        .wlroots = .{
            .url = "git+https://codeberg.org/ifreund/zig-wlroots#c183af18ae35dd5fb3dde709697c8f84bb1dc7f3",
            .hash = "wlroots-0.20.2-dev-jmOlcrJuBABMWPGDKK86BO7zrfmxm17nimqFb0gffZTn",
        },
        .xkbcommon = .{
            .url = "https://codeberg.org/ifreund/zig-xkbcommon/archive/v0.4.0.tar.gz",
            .hash = "xkbcommon-0.4.0-VDqIe0i2AgDRsok2GpMFYJ8SVhQS10_PI2M_CnHXsJJZ",
        },
        .translate_c = .{
            .url = "git+https://codeberg.org/ifreund/translate-c?ref=0.16.x#64570104eeed461dc221e77574cd3f1c5b7eb974",
            .hash = "translate_c-0.0.0-Q_BUWlL1BgD7PmL27vTsccyNCOvKb-B65O5Ki1ZKm-ED",
        },
    },
    .fingerprint = 0xf5e3672b8e8d6efc,
}
"#;

/// A manifest in the shape of a waylock-style package: a `.paths` filter, a
/// minimum zig version, a quoted dependency name, a local path dependency and
/// a lazy dependency. Written for this test, not copied from waylock.
const WAYLOCK_LIKE: &str = r#"
.{
    .name = .waylock,
    .version = "0.6.0",
    .fingerprint = 0x9e6e_a4bd_4ea2_fcc4,
    .minimum_zig_version = "0.14.0",
    .paths = .{ "build.zig", "build.zig.zon", "src", "LICENSE" },
    .dependencies = .{
        .@"zig-wayland" = .{
            .url = "https://codeberg.org/ifreund/zig-wayland/archive/v0.3.0.tar.gz",
            .hash = "wayland-0.3.0-AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        },
        .xkbcommon = .{ .path = "../xkbcommon" },
        .optional_tool = .{
            .url = "https://example.org/tool-1.0.tar.gz",
            .hash = "tool-1.0-BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB",
            .lazy = true,
        },
    },
}
"#;

fn dep<'a>(m: &'a Manifest, name: &str) -> &'a Dependency {
    m.dependencies
        .iter()
        .find(|d| d.name == name)
        .unwrap_or_else(|| panic!("no dependency {name}"))
}

#[test]
fn river_manifest_yields_every_dependency_with_its_url_and_hash() {
    let m = parse(RIVER).unwrap();
    assert_eq!(m.name.as_deref(), Some("river"));
    assert_eq!(m.version.as_deref(), Some("0.5.0"));
    assert_eq!(m.fingerprint, Some(0xf5e3_672b_8e8d_6efc));
    assert_eq!(m.paths, Some(vec![String::new()]));
    assert_eq!(m.minimum_zig_version, None);
    let names: Vec<&str> = m.dependencies.iter().map(|d| d.name.as_str()).collect();
    assert_eq!(
        names,
        ["pixman", "wayland", "wlroots", "xkbcommon", "translate_c"]
    );
    let pixman = dep(&m, "pixman");
    assert_eq!(
        pixman.url.as_deref(),
        Some("https://codeberg.org/ifreund/zig-pixman/archive/v0.3.0.tar.gz")
    );
    assert_eq!(
        pixman.hash.as_deref(),
        Some("pixman-0.3.0-LClMnz2VAAAs7QSCGwLimV5VUYx0JFnX5xWU6HwtMuDX")
    );
    assert!(!pixman.lazy && pixman.path.is_none());
}

#[test]
fn git_urls_keep_their_commit_and_ref_as_written() {
    let m = parse(RIVER).unwrap();
    assert_eq!(
        dep(&m, "wlroots").url.as_deref(),
        Some(
            "git+https://codeberg.org/ifreund/zig-wlroots#c183af18ae35dd5fb3dde709697c8f84bb1dc7f3"
        )
    );
    assert_eq!(
        dep(&m, "translate_c").url.as_deref(),
        Some(
            "git+https://codeberg.org/ifreund/translate-c?ref=0.16.x#64570104eeed461dc221e77574cd3f1c5b7eb974"
        )
    );
    // A hash whose base64 part contains `-` is one string, not split.
    assert_eq!(
        dep(&m, "wayland").hash.as_deref(),
        Some("wayland-0.6.0-lQa1kqz8AQADQmdNJsNhLoNHcnEGEUjrOaPV-dtEnEmX")
    );
}

#[test]
fn waylock_like_manifest_reads_paths_lazy_and_path_dependencies() {
    let m = parse(WAYLOCK_LIKE).unwrap();
    assert_eq!(m.fingerprint, Some(0x9e6e_a4bd_4ea2_fcc4));
    assert_eq!(m.minimum_zig_version.as_deref(), Some("0.14.0"));
    assert_eq!(
        m.paths.as_deref(),
        Some(
            &[
                "build.zig".to_string(),
                "build.zig.zon".to_string(),
                "src".to_string(),
                "LICENSE".to_string()
            ][..]
        )
    );
    let wayland = dep(&m, "zig-wayland");
    assert_eq!(
        wayland.url.as_deref().map(|u| u.ends_with("v0.3.0.tar.gz")),
        Some(true)
    );
    let local = dep(&m, "xkbcommon");
    assert_eq!(local.path.as_deref(), Some("../xkbcommon"));
    assert_eq!(local.url, None);
    let tool = dep(&m, "optional_tool");
    assert!(tool.lazy);
}

#[test]
fn absent_fields_are_none_and_empty_lists_are_empty() {
    let m = parse(".{ .name = \"x\", .version = \"1\", .dependencies = .{} }").unwrap();
    assert_eq!(m.paths, None);
    assert!(m.dependencies.is_empty());
    assert_eq!(m.fingerprint, None);
    let bare = parse(".{}").unwrap();
    assert_eq!(bare, Manifest::default());
}

#[test]
fn an_empty_paths_tuple_is_refused_as_zig_refuses_it() {
    // zig 0.15.2: "expected paths expression to be a list of strings".
    let err = parse(".{ .name = \"x\", .version = \"1\", .paths = .{} }").unwrap_err();
    assert!(err.contains("lists no path"), "{err}");
}

#[test]
fn string_escapes_decode_like_zig() {
    let m = parse(r#".{ .version = "a\"b\\c\x41\u{e9}\n\t" }"#).unwrap();
    assert_eq!(m.version.as_deref(), Some("a\"b\\cAé\n\t"));
}

#[test]
fn comments_and_trailing_commas_are_skipped() {
    let src = "// head\n.{ // open\n  .name = .x, // name\n  .paths = .{ \"a\", // one\n \"b\", },\n}\n// tail\n";
    let m = parse(src).unwrap();
    assert_eq!(m.name.as_deref(), Some("x"));
    assert_eq!(m.paths, Some(vec!["a".to_string(), "b".to_string()]));
}

#[test]
fn a_dependency_needs_exactly_one_of_url_and_path() {
    let both = parse(
        ".{ .dependencies = .{ .a = .{ .url = \"https://x/a.tar.gz\", .path = \"../a\" } } }",
    );
    assert!(both.unwrap_err().contains("both a url and a path"));
    let neither = parse(".{ .dependencies = .{ .a = .{ .hash = \"a-1-x\" } } }");
    assert!(neither.unwrap_err().contains("neither a url nor a path"));
}

#[test]
fn a_dependency_listed_twice_is_refused() {
    let src = ".{ .dependencies = .{ .a = .{ .path = \"x\" }, .a = .{ .path = \"y\" } } }";
    assert!(parse(src).unwrap_err().contains("listed twice"));
}

#[test]
fn malformed_manifests_are_refused_with_a_line() {
    let cases = [
        ".{ .name = \"open }",
        ".{ .name = \"a\nb\" }",
        ".{ .name = }",
        ".{ .name .x }",
        ".{ .version = \"\\q\" }",
        ".{ .name = .x } trailing",
        "zig",
        "[]",
        ".{ .name = .x",
    ];
    for src in cases {
        let err = parse(src).unwrap_err();
        assert!(err.starts_with("build.zig.zon line"), "{src:?}: {err}");
    }
    // Wrong field types are refused too, with the field named.
    let typed = [
        (".{ .paths = \"src\" }", "paths"),
        (".{ .fingerprint = \"0x1\" }", "fingerprint"),
        (
            ".{ .dependencies = .{ .a = .{ .path = \"x\", .lazy = 1 } } }",
            "lazy",
        ),
    ];
    for (src, field) in typed {
        let err = parse(src).unwrap_err();
        assert!(err.contains(field), "{src:?}: {err}");
    }
}

#[test]
fn nesting_is_bounded() {
    let deep = format!(".{{ .x = {}1{} }}", ".{ ".repeat(40), " }".repeat(40));
    assert!(parse(&deep).unwrap_err().contains("nests too deeply"));
}

#[test]
fn oversized_manifests_are_refused_before_parsing() {
    let big = " ".repeat(MAX_MANIFEST_BYTES + 1);
    assert!(parse(&big).unwrap_err().contains("larger than"));
}
