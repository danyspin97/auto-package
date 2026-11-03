//! The signature check of the new sources: a key the package's keyring has
//! passes, another key, a revoked key or a changed archive is refused, the
//! compressed form of an archive is verified over its decompressed stream, and
//! no gpg is a skip.

use super::keyring_accept::accept_command;
use super::source_signature::*;
use super::*;
use std::io::Write;
use std::process::{Command, Stdio};

const SIGNER_A: &str = "Signer A <a@example.invalid>";
const SIGNER_B: &str = "Signer B <b@example.invalid>";
const SIGNER_C: &str = "Signer C <c@example.invalid>";

/// A GnuPG home that is removed with its agent stopped.
struct Home(tempfile::TempDir);

impl Home {
    fn new() -> Self {
        Home(tempfile::tempdir().expect("a temporary directory"))
    }

    fn path(&self) -> &Path {
        self.0.path()
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = Command::new("gpgconf")
            .arg("--homedir")
            .arg(self.path())
            .args(["--kill", "gpg-agent"])
            .output();
    }
}

/// `gpg` with `args` in `home` (key generation needs the agent, so no
/// `--no-autostart` here). `GNUPGHOME` is set on the command only.
fn gnupg(home: &Home, args: &[&str]) -> std::process::Output {
    Command::new("gpg")
        .arg("--homedir")
        .arg(home.path())
        .args(["--batch", "--no-tty"])
        .args(args)
        .env("GNUPGHOME", home.path())
        .output()
        .expect("gpg runs")
}

/// Whether a `gpg` runs at all, in a home of its own (never the user's).
fn gpg_ready() -> bool {
    let home = Home::new();
    gpg_command(Path::new("gpg"), home.path())
        .arg("--version")
        .output()
        .is_ok()
}

/// Whether the tool that makes `compressed` (`gzip -c` and so on) runs here.
fn tool_ready(tool: &str) -> bool {
    Command::new(tool).arg("--version").output().is_ok()
}

fn succeeded(out: &std::process::Output) {
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A home holding a fresh passphrase-less key for `uid`.
fn signer(uid: &str) -> Home {
    let home = Home::new();
    succeeded(&gnupg(
        &home,
        &[
            "--pinentry-mode",
            "loopback",
            "--passphrase",
            "",
            "--quick-gen-key",
            uid,
            "default",
            "default",
            "never",
        ],
    ));
    home
}

/// A home holding a key for `uid` that expired in 2020, made at that time: a
/// signature it makes then verifies as expired (EXPKEYSIG) today.
fn expired_signer(uid: &str) -> Home {
    let home = Home::new();
    succeeded(&gnupg(
        &home,
        &[
            "--faked-system-time",
            SIGNED_AT,
            "--pinentry-mode",
            "loopback",
            "--passphrase",
            "",
            "--quick-gen-key",
            uid,
            "default",
            "default",
            "1d",
        ],
    ));
    home
}

/// The time an expired key was made and signed with.
const SIGNED_AT: &str = "20200101T120000";

/// The fingerprint of `uid`'s primary key in `home`.
fn fingerprint(home: &Home, uid: &str) -> String {
    let out = gnupg(home, &["--with-colons", "--list-keys", uid]);
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find(|l| l.starts_with("fpr:"))
        .and_then(|l| l.split(':').nth(9))
        .expect("a fingerprint")
        .to_string()
}

/// Revokes `uid`'s key in `home` (the revocation is saved to the key).
fn revoke(home: &Home, uid: &str) {
    let fpr = fingerprint(home, uid);
    let mut child = Command::new("gpg")
        .arg("--homedir")
        .arg(home.path())
        .args([
            "--no-tty",
            "--yes",
            "--command-fd",
            "0",
            "--pinentry-mode",
            "loopback",
            "--passphrase",
            "",
            "--edit-key",
            &fpr,
            "revkey",
        ])
        .env("GNUPGHOME", home.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("gpg runs");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(b"y\n0\n\ny\nsave\n")
        .expect("answers written");
    succeeded(&child.wait_with_output().expect("gpg finishes"));
}

/// Detached signature of `input` by `uid` in `home`, written to `output`; made
/// at `at` when given.
fn detach_sign(home: &Home, uid: &str, input: &Path, output: &Path, at: Option<&str>) {
    let mut cmd = Command::new("gpg");
    cmd.arg("--homedir")
        .arg(home.path())
        .args(["--batch", "--no-tty"]);
    if let Some(at) = at {
        cmd.args(["--faked-system-time", at]);
    }
    let out = cmd
        .args(["--local-user", uid, "--detach-sign", "--output"])
        .arg(output)
        .arg(input)
        .env("GNUPGHOME", home.path())
        .output()
        .expect("gpg runs");
    succeeded(&out);
}

/// The public keys of `keys` (home and uid each), as one keyring file's bytes.
fn keyring_of(keys: &[(&Home, &str)]) -> Vec<u8> {
    keys.iter()
        .flat_map(|(home, uid)| gnupg(home, &["--export", uid]).stdout)
        .collect()
}

/// A checkout of `pkg-1.0.tar.gz`, signed by `signer` as `uid`, with the
/// package's keyring holding the keys `keyring` names.
fn package(signer: &Home, uid: &str, keyring: &[(&Home, &str)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("a temporary directory");
    std::fs::write(dir.path().join("pkg-1.0.tar.gz"), "payload\n").expect("archive written");
    detach_sign(
        signer,
        uid,
        &dir.path().join("pkg-1.0.tar.gz"),
        &dir.path().join("pkg-1.0.tar.gz.sig"),
        None,
    );
    std::fs::write(dir.path().join("pkg.keyring"), keyring_of(keyring)).expect("keyring written");
    dir
}

/// A checkout of `pkg-1.0.tar` signed uncompressed, then compressed with `tool`
/// (`args` then the file) to `pkg-1.0.tar.<ext>`: the shape of a release whose
/// signature is over the decompressed tar.
fn compressed_package(
    signer: &Home,
    uid: &str,
    tool: &str,
    args: &[&str],
    ext: &str,
) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("a temporary directory");
    let tar = dir.path().join("pkg-1.0.tar");
    std::fs::write(&tar, "payload\n").expect("tar written");
    detach_sign(
        signer,
        uid,
        &tar,
        &dir.path().join("pkg-1.0.tar.sign"),
        None,
    );
    let out = Command::new(tool)
        .args(args)
        .arg(&tar)
        .output()
        .expect("compressor runs");
    succeeded(&out);
    std::fs::write(dir.path().join(format!("pkg-1.0.tar.{ext}")), &out.stdout)
        .expect("compressed written");
    std::fs::remove_file(&tar).expect("tar removed");
    dir
}

fn keyring_file(dir: &Path, name: &str, keys: &[(&Home, &str)]) {
    std::fs::write(dir.join(name), keyring_of(keys)).expect("keyring written");
}

#[test]
fn a_signature_by_the_keyrings_key_passes() {
    if !gpg_ready() {
        eprintln!("gpg is not installed: skipping (the keyring tests need gpg)");
        return;
    }
    let a = signer(SIGNER_A);
    let dir = package(&a, SIGNER_A, &[(&a, SIGNER_A)]);
    assert_eq!(check_with(Path::new("gpg"), dir.path()), SignatureCheck::Ok);
}

#[test]
fn a_signature_by_another_key_is_refused_with_its_fingerprint() {
    if !gpg_ready() {
        eprintln!("gpg is not installed: skipping (the keyring tests need gpg)");
        return;
    }
    let a = signer(SIGNER_A);
    let b = signer(SIGNER_B);
    let fpr_b = fingerprint(&b, SIGNER_B);
    // The keyring holds only A's key; the archive is signed by B.
    let dir = package(&b, SIGNER_B, &[(&a, SIGNER_A)]);
    let SignatureCheck::Refused(why) = check_with(Path::new("gpg"), dir.path()) else {
        panic!("a signature by another key passes");
    };
    assert!(why.contains(&fpr_b), "{why}");
    assert!(why.contains("is not in pkg.keyring"), "{why}");
    assert!(why.contains("a maintainer"), "{why}");
}

#[test]
fn a_changed_archive_is_refused() {
    if !gpg_ready() {
        eprintln!("gpg is not installed: skipping (the keyring tests need gpg)");
        return;
    }
    let a = signer(SIGNER_A);
    let dir = package(&a, SIGNER_A, &[(&a, SIGNER_A)]);
    std::fs::write(dir.path().join("pkg-1.0.tar.gz"), "tampered\n").expect("archive changed");
    let SignatureCheck::Refused(why) = check_with(Path::new("gpg"), dir.path()) else {
        panic!("a changed archive passes");
    };
    assert!(why.contains("does not verify"), "{why}");
}

#[test]
fn a_signature_over_the_decompressed_archive_is_verified() {
    if !gpg_ready() {
        eprintln!("gpg is not installed: skipping (the keyring tests need gpg)");
        return;
    }
    let a = signer(SIGNER_A);
    for (tool, args, ext) in [
        ("xz", &["-c"][..], "xz"),
        ("gzip", &["-c"][..], "gz"),
        ("bzip2", &["-c"][..], "bz2"),
        ("zstd", &["-c", "-q"][..], "zst"),
    ] {
        if !tool_ready(tool) {
            eprintln!("no {tool}: skipped");
            continue;
        }
        let dir = compressed_package(&a, SIGNER_A, tool, args, ext);
        keyring_file(dir.path(), "pkg-1.0.keyring", &[(&a, SIGNER_A)]);
        assert_eq!(
            check_with(Path::new("gpg"), dir.path()),
            SignatureCheck::Ok,
            "{tool}"
        );
    }
}

#[test]
fn a_decompressed_archive_signed_by_another_key_is_refused() {
    if !gpg_ready() || !tool_ready("xz") {
        eprintln!("no gpg or xz: skipped");
        return;
    }
    let a = signer(SIGNER_A);
    let b = signer(SIGNER_B);
    let fpr_b = fingerprint(&b, SIGNER_B);
    let dir = compressed_package(&b, SIGNER_B, "xz", &["-c"], "xz");
    keyring_file(dir.path(), "pkg.keyring", &[(&a, SIGNER_A)]);
    let SignatureCheck::Refused(why) = check_with(Path::new("gpg"), dir.path()) else {
        panic!("a decompressed archive signed by another key passes");
    };
    assert!(why.contains(&fpr_b), "{why}");
    assert!(why.contains("pkg-1.0.tar.xz"), "{why}");
}

#[test]
fn two_keyrings_are_refused_as_the_validator_does() {
    if !gpg_ready() {
        eprintln!("gpg is not installed: skipping (the keyring tests need gpg)");
        return;
    }
    let a = signer(SIGNER_A);
    let dir = package(&a, SIGNER_A, &[(&a, SIGNER_A)]);
    keyring_file(dir.path(), "other.keyring", &[(&a, SIGNER_A)]);
    let SignatureCheck::Refused(why) = check_with(Path::new("gpg"), dir.path()) else {
        panic!("two keyrings pass");
    };
    assert!(why.contains("2 keyrings"), "{why}");
    assert!(why.contains("other.keyring, pkg.keyring"), "{why}");
}

#[test]
fn a_signature_the_spec_does_not_name_is_checked_too() {
    if !gpg_ready() {
        eprintln!("gpg is not installed: skipping (the keyring tests need gpg)");
        return;
    }
    let a = signer(SIGNER_A);
    let b = signer(SIGNER_B);
    let dir = package(&a, SIGNER_A, &[(&a, SIGNER_A)]);
    // An old release left in the checkout, signed by a key the keyring lacks.
    std::fs::write(dir.path().join("old-0.9.tar.gz"), "old\n").expect("old archive written");
    detach_sign(
        &b,
        SIGNER_B,
        &dir.path().join("old-0.9.tar.gz"),
        &dir.path().join("old-0.9.tar.gz.asc"),
        None,
    );
    let SignatureCheck::Refused(why) = check_with(Path::new("gpg"), dir.path()) else {
        panic!("a signature the spec does not name was not checked");
    };
    assert!(
        why.contains("old-0.9.tar.gz.asc") || why.contains("old-0.9.tar.gz"),
        "{why}"
    );
}

#[test]
fn a_keyring_with_two_keys_verifies_either_signer() {
    if !gpg_ready() {
        eprintln!("gpg is not installed: skipping (the keyring tests need gpg)");
        return;
    }
    let a = signer(SIGNER_A);
    let c = signer(SIGNER_C);
    let dir = package(&c, SIGNER_C, &[(&a, SIGNER_A), (&c, SIGNER_C)]);
    assert_eq!(check_with(Path::new("gpg"), dir.path()), SignatureCheck::Ok);
}

#[test]
fn an_expired_signing_key_is_accepted_as_the_validator_accepts_it() {
    if !gpg_ready() {
        eprintln!("gpg is not installed: skipping (the keyring tests need gpg)");
        return;
    }
    let old = expired_signer(SIGNER_A);
    let dir = tempfile::tempdir().expect("a temporary directory");
    std::fs::write(dir.path().join("pkg-1.0.tar.gz"), "payload\n").expect("archive written");
    detach_sign(
        &old,
        SIGNER_A,
        &dir.path().join("pkg-1.0.tar.gz"),
        &dir.path().join("pkg-1.0.tar.gz.sig"),
        Some(SIGNED_AT),
    );
    keyring_file(dir.path(), "pkg.keyring", &[(&old, SIGNER_A)]);
    assert_eq!(check_with(Path::new("gpg"), dir.path()), SignatureCheck::Ok);
}

#[test]
fn a_signature_by_a_revoked_key_is_refused() {
    if !gpg_ready() {
        eprintln!("gpg is not installed: skipping (the keyring tests need gpg)");
        return;
    }
    let a = signer(SIGNER_A);
    let fpr = fingerprint(&a, SIGNER_A);
    // Signed before the revocation, then the key is revoked and exported with it.
    let dir = package(&a, SIGNER_A, &[]);
    revoke(&a, SIGNER_A);
    keyring_file(dir.path(), "pkg.keyring", &[(&a, SIGNER_A)]);
    let SignatureCheck::Refused(why) = check_with(Path::new("gpg"), dir.path()) else {
        panic!("a signature by a revoked key passes");
    };
    assert!(why.contains(&fpr), "{why}");
    assert!(why.contains("revoked"), "{why}");
}

#[test]
fn no_gpg_is_skipped_with_a_note() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    std::fs::write(dir.path().join("pkg-1.0.tar.gz"), "payload\n").expect("archive written");
    std::fs::write(dir.path().join("pkg-1.0.tar.gz.sig"), "sig\n").expect("sig written");
    std::fs::write(dir.path().join("pkg.keyring"), "key\n").expect("keyring written");
    let missing = Path::new("/nonexistent/auto-package-test-gpg");
    let SignatureCheck::Skipped(note) = check_with(missing, dir.path()) else {
        panic!("no gpg did not skip");
    };
    assert!(note.contains("gpg is not installed"), "{note}");
}

#[test]
fn an_unsigned_package_or_one_without_a_keyring_needs_no_gpg() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    std::fs::write(dir.path().join("pkg-1.0.tar.gz"), "payload\n").expect("archive written");
    let missing = Path::new("/nonexistent/auto-package-test-gpg");
    assert_eq!(check_with(missing, dir.path()), SignatureCheck::Ok);
    std::fs::write(dir.path().join("pkg-1.0.tar.gz.sig"), "sig\n").expect("sig written");
    let SignatureCheck::Skipped(note) = check_with(missing, dir.path()) else {
        panic!("a signature without a keyring was not skipped");
    };
    assert!(note.contains("has no keyring"), "{note}");
}

#[test]
fn gpg_is_only_ever_run_in_its_own_home() {
    let home = Path::new("/tmp/auto-package-gpg-test-home");
    let cmd = gpg_command(Path::new("gpg"), home);
    let args: Vec<_> = cmd.get_args().collect();
    assert!(
        args.windows(2)
            .any(|w| w[0] == "--homedir" && w[1] == home.as_os_str())
    );
    for flag in [
        "--no-auto-key-retrieve",
        "--no-auto-key-locate",
        "--no-autostart",
    ] {
        assert!(args.iter().any(|a| *a == flag), "{flag}");
    }
    let env: Vec<_> = cmd.get_envs().collect();
    assert!(
        env.iter()
            .any(|(k, v)| *k == "GNUPGHOME" && *v == Some(home.as_os_str()))
    );
}

/// The dbus-1 shape: the keyring holds only an old key, now expired, and the
/// archive is signed by a new key. The refusal names the new fingerprint, what
/// the keyring holds (with the expiry), where to check the fingerprint and the
/// accept command.
#[test]
fn a_new_key_refusal_says_what_the_keyring_holds() {
    if !gpg_ready() {
        eprintln!("gpg is not installed: skipping (the keyring tests need gpg)");
        return;
    }
    let old = expired_signer(SIGNER_A);
    let new = signer(SIGNER_B);
    let fpr_old = fingerprint(&old, SIGNER_A);
    let fpr_new = fingerprint(&new, SIGNER_B);
    let dir = package(&new, SIGNER_B, &[(&old, SIGNER_A)]);
    let SignatureCheck::Refused(why) = check_with(Path::new("gpg"), dir.path()) else {
        panic!("a signature by a key the keyring lacks passes");
    };
    assert!(why.contains(&fpr_new), "{why}");
    assert!(
        why.contains(&format!("it holds: {fpr_old} ({SIGNER_A}, expired")),
        "{why}"
    );
    assert!(why.contains("announcement or website"), "{why}");
    assert!(why.contains(&accept_command(dir.path(), &fpr_new)), "{why}");
    assert!(why.contains("so none is known here"), "{why}");
}

#[test]
fn the_keyring_listing_is_sanitised_and_says_the_expiry_date() {
    use super::keyring_listing::{parse_holds, printable};
    // A C1 control (U+009B) and a right-to-left override (U+202E), raw and as
    // gpg's \x escapes, and an escaped colon.
    let raw = "Eve\u{85}\u{202e}moc <a\\x3ab@x>\\xc2\\x9b";
    assert_eq!(printable(raw), "Evemoc <a:b@x>");
    // The separators of the listing cannot forge an entry, and the separators,
    // line and paragraph ends and zero-width marks are dropped or replaced.
    assert_eq!(printable("x); FPR (y"), "x__ FPR _y");
    assert_eq!(
        printable("a\u{2028}b\u{2029}c\u{200e}d\u{200f}e\u{200b}f\u{061c}g"),
        "abcdefg"
    );
    let colons = "pub:e:4096:1:AAAA:1000000000:1700000000::u:::scESC:\n\
                  fpr:::::::::0123456789ABCDEF0123456789ABCDEF01234567:\n\
                  uid:e::::1000000000::H::Evil\\xe2\\x80\\xae <e@x>::::::::::0:\n\
                  pub:v:4096:1:BBBB:1000000000:1900000000::u:::scESC:\n\
                  fpr:::::::::FEDCBA9876543210FEDCBA9876543210FEDCBA98:\n\
                  uid:r::::1000000000::H::Old <old@x>::::::::::0:\n";
    assert_eq!(
        parse_holds(colons),
        "0123456789ABCDEF0123456789ABCDEF01234567 (Evil <e@x>, expired on 2023-11-14); \
         FEDCBA9876543210FEDCBA9876543210FEDCBA98 (Old <old@x> (revoked), expires 2030-03-17)"
    );
}

#[test]
fn a_user_id_cannot_hide_text_fake_a_code_span_or_fill_the_refusal() {
    use super::keyring_listing::printable;
    // Every invisible character the listing drops, each between two letters.
    let hidden = [
        '\u{00AD}',
        '\u{034F}',
        '\u{115F}',
        '\u{1160}',
        '\u{180B}',
        '\u{180E}',
        '\u{2065}',
        '\u{206A}',
        '\u{206F}',
        '\u{2800}',
        '\u{3164}',
        '\u{FE00}',
        '\u{FE0F}',
        '\u{FFA0}',
        '\u{FFF9}',
        '\u{FFFC}',
        '\u{E0001}',
        '\u{E0041}',
        '\u{E007F}',
        '\u{E0100}',
    ];
    for c in hidden {
        assert_eq!(printable(&format!("a{c}b")), "ab", "U+{:04X}", c as u32);
    }
    // A backtick or a quote cannot open a code span or close a string.
    assert_eq!(printable("x `ap accept y` \"z\""), "x _ap accept y_ _z_");
    // A name of 64 KB is cut, with the cut said.
    let long = "n".repeat(65_536);
    let cut = printable(&long);
    assert_eq!(cut.chars().count(), 83, "{cut}");
    assert!(cut.ends_with("n..."), "{cut}");
    // A name at the limit is whole.
    assert_eq!(printable(&"n".repeat(80)), "n".repeat(80));
}
