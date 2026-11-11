//! The rendering of the doctor's report against fixed rows: the goldens of
//! both hosts and widths, the escapes, the columns, the install line, the
//! verdict and the JSON. Nothing is run; the rows are built here.

use ap_build::envcheck::{CheckResult, Outcome, Required};
use ap_core::remedy::Cause;
use std::path::PathBuf;

use crate::doctor_cmd::{JsonReport, exit_code, json_report, render_json, render_text};

const OFFLINE: &str = "auto-package doctor: environment check (offline)";

fn row(
    id: &str,
    name: &str,
    required: Required,
    outcome: Outcome,
    hint: Option<&str>,
    package: Option<&'static str>,
) -> CheckResult {
    CheckResult {
        id: id.to_string(),
        name: name.to_string(),
        required,
        outcome,
        hint: hint.map(str::to_string),
        package,
    }
}

fn missing_tool(tool: &str, path: Option<&str>) -> Cause {
    Cause::MissingTool {
        tool: tool.to_string(),
        path: path.map(PathBuf::from),
    }
}

/// A mixed set on an openSUSE host: ok, missing, warn, missing-text and
/// warn-text rows, with the install hints the checker gives there.
fn mixed_suse() -> Vec<CheckResult> {
    vec![
        row(
            "osc",
            "osc on PATH",
            Required::Yes,
            Outcome::Ok("/usr/bin/osc".to_string()),
            None,
            None,
        ),
        row(
            "rpmspec",
            "rpmspec on PATH",
            Required::Yes,
            Outcome::Missing(missing_tool("rpmspec", None)),
            Some("sudo zypper install rpm-build"),
            Some("rpm-build"),
        ),
        row(
            "quilt",
            "quilt on PATH",
            Required::Yes,
            Outcome::Missing(missing_tool("quilt", None)),
            Some("sudo zypper install quilt"),
            Some("quilt"),
        ),
        row(
            "gpg",
            "gpg on PATH",
            Required::No,
            Outcome::Warn(missing_tool("gpg", Some("/usr/bin/gpg"))),
            Some("sudo zypper install gpg2"),
            Some("gpg2"),
        ),
        row(
            "sandbox",
            "service sandbox (bubblewrap)",
            Required::Yes,
            Outcome::MissingText("bubblewrap cannot create user namespaces here".to_string()),
            None,
            None,
        ),
        row(
            "github-token",
            "GitHub token",
            Required::No,
            Outcome::WarnText("GITHUB_TOKEN is not set and no gh login was found".to_string()),
            None,
            None,
        ),
    ]
}

/// The same mixed set on a host that is not openSUSE: prose hints, no package.
fn mixed_other() -> Vec<CheckResult> {
    let mut rows = mixed_suse();
    for r in &mut rows {
        r.package = None;
        r.hint = match r.id.as_str() {
            "rpmspec" => Some("install the package that provides rpmspec".to_string()),
            "quilt" => Some("install the package that provides quilt".to_string()),
            "gpg" => Some("install the package that provides gpg".to_string()),
            _ => None,
        };
    }
    rows
}

/// The SGR escapes removed: what a terminal shows of coloured text.
fn strip(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            for d in chars.by_ref() {
                if d == 'm' {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

#[test]
fn the_plain_text_of_a_mixed_set_on_suse_at_100_columns() {
    let expected = "\
auto-package doctor: environment check (offline)

  ✓ osc on PATH                   required  /usr/bin/osc
  ✗ rpmspec on PATH               required  rpmspec is not installed
      sudo zypper install rpm-build
  ✗ quilt on PATH                 required  quilt is not installed
      sudo zypper install quilt
  ⚠ gpg on PATH                   optional  gpg is not installed (/usr/bin/gpg is missing)
      sudo zypper install gpg2
  ✗ service sandbox (bubblewrap)  required  bubblewrap cannot create user namespaces here
  ⚠ GitHub token                  optional  GITHUB_TOKEN is not set and no gh login was found

Install everything missing with:
sudo zypper install quilt rpm-build

3 required item(s) missing.
";
    assert_eq!(
        render_text(&mixed_suse(), OFFLINE, false, 100, true),
        expected
    );
}

#[test]
fn the_plain_text_of_a_mixed_set_at_60_columns_stacks_the_detail() {
    let expected = "\
auto-package doctor: environment check (offline)

  ✓ osc on PATH  required
    /usr/bin/osc
  ✗ rpmspec on PATH  required
    rpmspec is not installed
      sudo zypper install rpm-build
  ✗ quilt on PATH  required
    quilt is not installed
      sudo zypper install quilt
  ⚠ gpg on PATH  optional
    gpg is not installed (/usr/bin/gpg is missing)
      sudo zypper install gpg2
  ✗ service sandbox (bubblewrap)  required
    bubblewrap cannot create user namespaces here
  ⚠ GitHub token  optional
    GITHUB_TOKEN is not set and no gh login was found

Install everything missing with:
sudo zypper install quilt rpm-build

3 required item(s) missing.
";
    assert_eq!(
        render_text(&mixed_suse(), OFFLINE, false, 60, true),
        expected
    );
}

#[test]
fn the_plain_text_of_a_mixed_set_at_40_columns_stacks_and_wraps_the_detail() {
    let expected = "\
auto-package doctor: environment check (offline)

  ✓ osc on PATH  required
    /usr/bin/osc
  ✗ rpmspec on PATH  required
    rpmspec is not installed
      sudo zypper install rpm-build
  ✗ quilt on PATH  required
    quilt is not installed
      sudo zypper install quilt
  ⚠ gpg on PATH  optional
    gpg is not installed (/usr/bin/gpg
    is missing)
      sudo zypper install gpg2
  ✗ service sandbox (bubblewrap)  required
    bubblewrap cannot create user
    namespaces here
  ⚠ GitHub token  optional
    GITHUB_TOKEN is not set and no gh
    login was found

Install everything missing with:
sudo zypper install quilt rpm-build

3 required item(s) missing.
";
    assert_eq!(
        render_text(&mixed_suse(), OFFLINE, false, 40, true),
        expected
    );
}

#[test]
fn an_unchecked_required_row_is_ready_with_a_note_and_counted() {
    let rows = vec![row(
        "claude",
        "claude logged in",
        Required::Yes,
        Outcome::Unchecked("claude auth status could not run: timed out".to_string()),
        None,
        None,
    )];
    assert_eq!(exit_code(&rows), 0);
    assert!(
        render_text(&rows, OFFLINE, false, 100, true)
            .ends_with("\nReady, but 1 required check(s) could not be run.\n")
    );
    assert_eq!(json_report(&rows).unchecked, 1);
    assert!(json_report(&rows).ready);
    // An optional unchecked row is a warning, not a note on the verdict.
    let optional = vec![row(
        "github-token",
        "GitHub token",
        Required::No,
        Outcome::Unchecked("x".to_string()),
        None,
        None,
    )];
    assert!(render_text(&optional, OFFLINE, false, 100, true).ends_with("\nReady.\n"));
    assert_eq!(json_report(&optional).unchecked, 0);
    // A missing required row still fails over an unchecked one.
    let both = vec![rows[0].clone(), mixed_suse().remove(1)];
    assert_eq!(exit_code(&both), 1);
    assert!(
        render_text(&both, OFFLINE, false, 100, true).ends_with("\n1 required item(s) missing.\n")
    );
}

#[test]
fn environment_text_is_printable_in_the_report() {
    let rows = vec![row(
        "oscrc",
        "an oscrc file",
        Required::Yes,
        Outcome::Ok("oscrc at /home/x\u{1b}[31my/oscrc\nnext".to_string()),
        None,
        None,
    )];
    let text = render_text(&rows, OFFLINE, false, 100, true);
    assert!(!text.contains('\x1b'));
    assert!(text.contains("oscrc at /home/x?[31my/oscrc?next"));
    assert!(!render_json(&rows).contains('\x1b'));
}

#[test]
fn the_plain_text_of_a_mixed_set_off_suse_has_prose_hints_and_a_note() {
    let expected = "\
auto-package doctor: environment check (offline)

  ✓ osc on PATH                   required  /usr/bin/osc
  ✗ rpmspec on PATH               required  rpmspec is not installed
      install the package that provides rpmspec
  ✗ quilt on PATH                 required  quilt is not installed
      install the package that provides quilt
  ⚠ gpg on PATH                   optional  gpg is not installed (/usr/bin/gpg is missing)
      install the package that provides gpg
  ✗ service sandbox (bubblewrap)  required  bubblewrap cannot create user namespaces here
  ⚠ GitHub token                  optional  GITHUB_TOKEN is not set and no gh login was found

Package names are openSUSE's; on this host, look up the equivalents.

3 required item(s) missing.
";
    assert_eq!(
        render_text(&mixed_other(), OFFLINE, false, 100, false),
        expected
    );
}

#[test]
fn the_coloured_text_uses_the_palette_codes() {
    let text = render_text(&mixed_suse(), OFFLINE, true, 100, true);
    assert!(text.starts_with("\x1b[1mauto-package doctor: environment check (offline)\x1b[22m\n"));
    assert!(text.contains("  \x1b[32m✓\x1b[39m osc on PATH"));
    assert!(text.contains("  \x1b[1;31m✗\x1b[22;39m rpmspec on PATH"));
    assert!(text.contains("  \x1b[33m⚠\x1b[39m gpg on PATH"));
    assert!(text.contains("\x1b[2m/usr/bin/osc\x1b[22m\n"));
    assert!(text.contains("      \x1b[32msudo zypper install rpm-build\x1b[39m\n"));
    assert!(text.contains("\x1b[32msudo zypper install quilt rpm-build\x1b[39m\n"));
    assert!(text.ends_with("\x1b[1;31m3 required item(s) missing.\x1b[22;39m\n"));
}

#[test]
fn stripping_the_escapes_gives_the_plain_text() {
    for results in [mixed_suse(), mixed_other()] {
        for suse in [true, false] {
            for width in [40, 60, 100, 140] {
                let coloured = render_text(&results, OFFLINE, true, width, suse);
                let plain = render_text(&results, OFFLINE, false, width, suse);
                assert!(coloured.contains('\x1b'));
                assert!(!plain.contains('\x1b'));
                assert_eq!(strip(&coloured), plain, "width {width}, suse {suse}");
            }
        }
    }
}

#[test]
fn the_values_of_a_column_line_up() {
    let text = render_text(&mixed_suse(), OFFLINE, false, 100, true);
    let columns: Vec<usize> = text
        .lines()
        // A row is indented by two columns and then its glyph; the verdict
        // line also says "required".
        .filter(|line| line.starts_with("  ") && !line.starts_with("   "))
        .filter_map(|line| {
            line.find("required")
                .or_else(|| line.find("optional"))
                .map(|at| line[..at].chars().count())
        })
        .collect();
    assert_eq!(columns.len(), 6);
    assert!(columns.iter().all(|&c| c == columns[0]), "{columns:?}");
}

#[test]
fn the_install_line_is_one_exact_line_and_hints_are_kept() {
    let text = render_text(&mixed_suse(), OFFLINE, false, 100, true);
    assert!(
        text.lines()
            .any(|line| line == "sudo zypper install quilt rpm-build")
    );
    assert!(
        text.lines()
            .any(|line| line == "      sudo zypper install rpm-build")
    );
    assert!(!render_text(&mixed_other(), OFFLINE, false, 100, false).contains("sudo zypper"));
}

#[test]
fn a_missing_required_row_fails_and_warnings_do_not() {
    let results = mixed_suse();
    assert_eq!(exit_code(&results), 1);
    assert!(
        render_text(&results, OFFLINE, false, 100, true).ends_with("3 required item(s) missing.\n")
    );

    let warnings_only = vec![results[0].clone(), results[3].clone(), results[5].clone()];
    assert_eq!(exit_code(&warnings_only), 0);
    assert!(render_text(&warnings_only, OFFLINE, false, 100, true).ends_with("\nReady.\n"));

    let optional_missing = vec![row(
        "gpg",
        "gpg on PATH",
        Required::No,
        Outcome::Missing(missing_tool("gpg", None)),
        None,
        None,
    )];
    assert_eq!(exit_code(&optional_missing), 0);
    assert!(render_text(&optional_missing, OFFLINE, false, 100, true).ends_with("\nReady.\n"));

    let text_only = vec![row(
        "sandbox",
        "service sandbox (bubblewrap)",
        Required::Yes,
        Outcome::MissingText("no sandbox".to_string()),
        None,
        None,
    )];
    assert_eq!(exit_code(&text_only), 1);
    assert!(
        render_text(&text_only, OFFLINE, false, 100, true)
            .ends_with("\n1 required item(s) missing.\n")
    );
}

#[test]
fn the_json_has_no_escapes_and_round_trips() {
    let results = mixed_suse();
    let text = render_json(&results);
    assert!(!text.contains('\x1b'));
    let back: JsonReport = serde_json::from_str(&text).unwrap();
    assert_eq!(back, json_report(&results));
    assert_eq!(
        back.zypper.as_deref(),
        Some("sudo zypper install quilt rpm-build")
    );
    assert!(!back.ready);
    let statuses: Vec<&str> = back.checks.iter().map(|c| c.status.as_str()).collect();
    assert_eq!(
        statuses,
        ["ok", "missing", "missing", "warn", "missing", "warn"]
    );
    let value: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(value["ready"], false);
    assert_eq!(value["checks"][1]["id"], "rpmspec");
    assert_eq!(value["checks"][1]["required"], true);
    assert_eq!(value["checks"][1]["package"], "rpm-build");
    assert_eq!(value["checks"][1]["detail"], "rpmspec is not installed");
    assert!(value["checks"][0]["hint"].is_null());
}

#[test]
fn the_json_of_a_ready_run_is_ready() {
    let report = json_report(&[mixed_suse().remove(0)]);
    assert!(report.ready);
    assert_eq!(report.zypper, None);
}
