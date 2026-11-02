//! `keyring accept`: a key is taken only when its full fingerprint is the one
//! typed, it signed the checkout's archive, and it is written with the change
//! recorded. The keys are local test keys in homes of their own: no network,
//! no keyserver, no osc.

use super::keyring_accept::*;
use super::source_signature::{SignatureCheck, check_with};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const SIGNER_A: &str = "Signer A <a@example.invalid>";
const SIGNER_B: &str = "Signer B <b@example.invalid>";

fn gnupg(home: &Path, args: &[&str]) -> Output {
    Command::new("gpg")
        .arg("--homedir")
        .arg(home)
        .args([
            "--batch",
            "--no-tty",
            "--pinentry-mode",
            "loopback",
            "--passphrase",
            "",
        ])
        .args(args)
        .env("GNUPGHOME", home)
        .output()
        .expect("gpg runs")
}

fn succeeded(out: &Output) {
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
        home.path(),
        &["--quick-gen-key", uid, "default", "default", "never"],
    ));
    home
}

/// The primary fingerprint of the only key in `home`.
fn fingerprint(home: &Path) -> String {
    let out = gnupg(home, &["--with-colons", "--list-keys"]);
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .skip_while(|l| !l.starts_with("pub:"))
        .find(|l| l.starts_with("fpr:"))
        .and_then(|l| l.split(':').nth(9))
        .expect("a fingerprint")
        .to_string()
}

/// The armored public key of `fpr` in `home`.
fn armored(home: &Path, fpr: &str) -> String {
    let out = gnupg(home, &["--armor", "--export", fpr]);
    succeeded(&out);
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Imports armored keys `text` into `home`.
fn import_armored(home: &Path, text: &str) {
    let file = home.join("import.asc");
    std::fs::write(&file, text).expect("armored key");
    succeeded(&gnupg(home, &["--import", file.to_str().unwrap()]));
}

/// A checkout `<tmp>/proj-x86_64/proj/pkg` holding `pkg-1.0.tar.gz` and its
/// detached signature made by `home`'s key.
struct Checkout {
    _root: tempfile::TempDir,
    dir: PathBuf,
}

fn checkout_signed_by(home: &Home) -> Checkout {
    let root = tempfile::tempdir().expect("a temporary directory");
    let dir = root.path().join("proj-x86_64").join("proj").join("pkg");
    std::fs::create_dir_all(&dir).expect("checkout");
    std::fs::write(dir.join("pkg-1.0.tar.gz"), b"upstream release").expect("archive");
    succeeded(&gnupg(
        home.path(),
        &[
            "--detach-sign",
            "--armor",
            "--output",
            dir.join("pkg-1.0.tar.gz.asc").to_str().unwrap(),
            dir.join("pkg-1.0.tar.gz").to_str().unwrap(),
        ],
    ));
    Checkout { _root: root, dir }
}

/// A GnuPG home of its own, removed with its agents stopped.
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
            .args(["--kill", "all"])
            .output();
    }
}

fn gpg() -> &'static Path {
    Path::new("gpg")
}

#[test]
fn fingerprint_must_be_the_full_one() {
    let fpr = "0123456789ABCDEF0123456789ABCDEF01234567";
    assert_eq!(parse_fingerprint(fpr).unwrap(), fpr);
    let spaced = "0123 4567 89ab cdef 0123 4567 89AB CDEF 0123 4567";
    assert_eq!(parse_fingerprint(spaced).unwrap(), fpr);
    assert_eq!(parse_fingerprint(&format!("0x{fpr}")).unwrap(), fpr);
    let short = parse_fingerprint("89ABCDEF01234567").unwrap_err();
    assert!(short.contains("short key ID"), "{short}");
    let long = parse_fingerprint(&format!("{fpr}0")).unwrap_err();
    assert!(long.contains("not a full fingerprint"), "{long}");
    assert!(parse_fingerprint(&"Z".repeat(40)).is_err());
}

#[test]
fn accepted_key_becomes_the_keyring_and_the_archive_verifies() {
    let a = signer(SIGNER_A);
    let fpr = fingerprint(a.path());
    let checkout = checkout_signed_by(&a);
    let accepted = accept_key_files(&checkout.dir, &fpr, a.path(), gpg()).expect("accepted");
    assert_eq!(accepted.keyring, "pkg.keyring");
    let text = std::fs::read_to_string(checkout.dir.join("pkg.keyring")).unwrap();
    assert_eq!(text.matches("BEGIN PGP PUBLIC KEY BLOCK").count(), 1);
    assert_eq!(check_with(gpg(), &checkout.dir), SignatureCheck::Ok);
}

#[test]
fn accepted_key_extends_an_existing_keyring() {
    let a = signer(SIGNER_A);
    let b = signer(SIGNER_B);
    let fpr_a = fingerprint(a.path());
    let fpr_b = fingerprint(b.path());
    let checkout = checkout_signed_by(&a);
    std::fs::write(checkout.dir.join("pkg.keyring"), armored(b.path(), &fpr_b)).unwrap();
    accept_key_files(&checkout.dir, &fpr_a, a.path(), gpg()).expect("accepted");
    let text = std::fs::read_to_string(checkout.dir.join("pkg.keyring")).unwrap();
    assert_eq!(text.matches("BEGIN PGP PUBLIC KEY BLOCK").count(), 2);
    assert_eq!(check_with(gpg(), &checkout.dir), SignatureCheck::Ok);
}

#[test]
fn only_the_named_key_is_exported_from_a_home_holding_two() {
    let a = signer(SIGNER_A);
    let b = signer(SIGNER_B);
    let fpr_a = fingerprint(a.path());
    let fpr_b = fingerprint(b.path());
    // Both keys in one home: accepting A must not bring B along.
    let both = Home::new();
    import_armored(both.path(), &armored(a.path(), &fpr_a));
    import_armored(both.path(), &armored(b.path(), &fpr_b));
    let checkout = checkout_signed_by(&a);
    accept_key_files(&checkout.dir, &fpr_a, both.path(), gpg()).expect("accepted");
    let text = std::fs::read_to_string(checkout.dir.join("pkg.keyring")).unwrap();
    assert_eq!(text.matches("BEGIN PGP PUBLIC KEY BLOCK").count(), 1);
    assert!(
        !text.contains(
            &armored(b.path(), &fpr_b)
                .lines()
                .nth(2)
                .unwrap()
                .to_string()
        )
    );
}

#[test]
fn a_key_that_did_not_sign_is_refused_and_nothing_is_written() {
    let a = signer(SIGNER_A);
    let b = signer(SIGNER_B);
    let fpr_a = fingerprint(a.path());
    let fpr_b = fingerprint(b.path());
    let checkout = checkout_signed_by(&b);
    let both = Home::new();
    import_armored(both.path(), &armored(b.path(), &fpr_b));
    import_armored(both.path(), &armored(a.path(), &fpr_a));
    // Accepting A (the key that did not sign) must fail; B signed.
    let err = accept_key_files(&checkout.dir, &fpr_a, both.path(), gpg()).unwrap_err();
    assert!(
        err.contains("nothing is accepted") || err.contains("does not verify"),
        "{err}"
    );
    assert!(!checkout.dir.join("pkg.keyring").exists());
    // And B is accepted when it is the one typed.
    accept_key_files(&checkout.dir, &fpr_b, both.path(), gpg()).expect("B accepted");
}

#[test]
fn a_home_without_the_named_key_is_refused() {
    let a = signer(SIGNER_A);
    let b = signer(SIGNER_B);
    let fpr_a = fingerprint(a.path());
    let checkout = checkout_signed_by(&a);
    let err = accept_key_files(&checkout.dir, &fpr_a, b.path(), gpg()).unwrap_err();
    assert!(err.contains("no key"), "{err}");
    assert!(!checkout.dir.join("pkg.keyring").exists());
}

#[test]
fn a_short_id_is_refused_before_any_key_is_read() {
    let a = signer(SIGNER_A);
    let checkout = checkout_signed_by(&a);
    let fpr = fingerprint(a.path());
    let err = accept_key_files(&checkout.dir, &fpr[24..], a.path(), gpg()).unwrap_err();
    assert!(err.contains("short key ID"), "{err}");
    assert!(!checkout.dir.join("pkg.keyring").exists());
}

#[test]
fn a_key_already_in_the_keyring_is_not_added_twice() {
    let a = signer(SIGNER_A);
    let fpr = fingerprint(a.path());
    let checkout = checkout_signed_by(&a);
    std::fs::write(checkout.dir.join("pkg.keyring"), armored(a.path(), &fpr)).unwrap();
    let err = accept_key_files(&checkout.dir, &fpr, a.path(), gpg()).unwrap_err();
    assert!(err.contains("already in pkg.keyring"), "{err}");
}

#[test]
fn needs_maintainer_text_names_the_fingerprint_and_the_command() {
    let a = signer(SIGNER_A);
    let b = signer(SIGNER_B);
    let fpr_a = fingerprint(a.path());
    let fpr_b = fingerprint(b.path());
    let checkout = checkout_signed_by(&a);
    std::fs::write(checkout.dir.join("pkg.keyring"), armored(b.path(), &fpr_b)).unwrap();
    let SignatureCheck::Refused(why) = check_with(gpg(), &checkout.dir) else {
        panic!("a key the keyring lacks must be refused");
    };
    assert!(why.contains(&fpr_a), "{why}");
    assert!(
        why.contains("from the issuer fingerprint of pkg-1.0.tar.gz.asc"),
        "{why}"
    );
    let command = accept_command(&checkout.dir, &fpr_a);
    assert!(why.contains(&command), "{why}");
    assert!(
        command.starts_with("auto-package keyring accept pkg --fingerprint "),
        "{command}"
    );
    assert!(command.contains(" --branch-project proj "), "{command}");
    assert!(command.contains(" --arch x86_64"), "{command}");
}
