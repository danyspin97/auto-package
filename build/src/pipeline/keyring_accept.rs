//! `keyring accept`: the one way a signing key that the package's keyring lacks
//! gets into it. The tool never trusts a key by itself. The user types the full
//! fingerprint, only the key whose primary fingerprint equals it is taken, every
//! signature of the checkout must verify with that key alone, and the change is
//! recorded in the `.changes` file through `osc vc`.

use super::source_signature::{
    SIGNATURE_SUFFIXES, archive_of, file_names, first_line, gpg_command,
};
use crate::osc;
use crate::proc::Emit;
use ap_core::fsx;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The full fingerprint the user typed, checked: 40 hex digits once spaces are
/// dropped, the case folded and a `0x` prefix removed. A short key ID is refused.
pub fn parse_fingerprint(input: &str) -> Result<String, String> {
    let mut fpr: String = input
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect::<String>()
        .to_ascii_uppercase();
    if let Some(rest) = fpr.strip_prefix("0X") {
        fpr = rest.to_string();
    }
    if fpr.len() == 40 && fpr.chars().all(|c| c.is_ascii_hexdigit()) {
        Ok(fpr)
    } else if fpr.len() == 16 && fpr.chars().all(|c| c.is_ascii_hexdigit()) {
        Err(format!(
            "{fpr} is a short key ID: give the full 40-digit fingerprint, a short ID can be forged"
        ))
    } else {
        Err(format!(
            "{input:?} is not a full fingerprint: expected 40 hex digits, got {} after removing spaces",
            fpr.len()
        ))
    }
}

/// The exact command that accepts `fingerprint` for the checkout `checkout`
/// (`<work>/<branch>-<arch>/<branch>/<package>`), as `keyring accept` takes it.
pub fn accept_command(checkout: &Path, fingerprint: &str) -> String {
    let package = name_of(checkout);
    let branch = checkout.parent().map(name_of).unwrap_or_default();
    let project_dir = checkout.parent().and_then(Path::parent);
    let project_dir_name = project_dir.map(name_of).unwrap_or_default();
    let arch = project_dir_name
        .strip_prefix(&format!("{}-", ap_core::run::sanitize_name(&branch)))
        .unwrap_or("x86_64")
        .to_string();
    let work = project_dir
        .and_then(Path::parent)
        .map(|w| std::path::absolute(w).unwrap_or_else(|_| w.to_path_buf()))
        .unwrap_or_default();
    format!(
        "auto-package keyring accept {package} --fingerprint {fingerprint} --branch-project {branch} --work-dir {} --arch {arch}",
        work.display()
    )
}

/// The result of an accepted key: the fingerprint, the keyring file it went
/// into and what that file held before (for a restore).
#[derive(Debug)]
pub struct Accepted {
    pub fingerprint: String,
    pub keyring: String,
    previous: Option<String>,
}

/// Fetches the key `fingerprint` from `keyserver` into a home of its own and
/// accepts it into the checkout's keyring. A key that the keyserver answers with
/// under another fingerprint is refused.
pub fn fetch_and_accept(
    checkout: &Path,
    fingerprint: &str,
    keyserver: &str,
    emit: Emit<'_>,
) -> Result<Accepted, String> {
    let fpr = parse_fingerprint(fingerprint)?;
    let home = Home::new()?;
    let out = Command::new("gpg")
        .arg("--homedir")
        .arg(home.path())
        .args([
            "--batch",
            "--no-tty",
            "--keyserver",
            keyserver,
            "--recv-keys",
        ])
        .arg(format!("0x{fpr}"))
        .env("GNUPGHOME", home.path())
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("gpg cannot run ({e})"))?;
    if !out.status.success() {
        return Err(format!(
            "no key {fpr} from {keyserver}: {}",
            first_line(&out.stderr)
        ));
    }
    accept_from_home(checkout, &fpr, home.path(), Path::new("gpg"), emit)
}

/// [`accept_key_files`], then the `.changes` entry through `osc vc`. When the
/// entry cannot be added the keyring is put back as it was.
pub fn accept_from_home(
    checkout: &Path,
    fingerprint: &str,
    source_home: &Path,
    gpg: &Path,
    emit: Emit<'_>,
) -> Result<Accepted, String> {
    let accepted = accept_key_files(checkout, fingerprint, source_home, gpg)?;
    let message = format!(
        "Accept signing key {} for {}",
        accepted.fingerprint, accepted.keyring
    );
    if let Err(e) = osc::add_changes_entry(checkout, &message, emit) {
        restore_keyring(checkout, &accepted);
        return Err(format!(
            "the .changes entry cannot be added ({e}); the keyring is left as it was"
        ));
    }
    Ok(accepted)
}

/// Puts the keyring of an accepted key back as it was before.
pub fn restore_keyring(checkout: &Path, accepted: &Accepted) {
    let path = checkout.join(&accepted.keyring);
    match &accepted.previous {
        Some(text) => {
            let _ = fsx::write_atomic(&path, text);
        }
        None => {
            let _ = fs::remove_file(&path);
        }
    }
}

/// Takes the key `fingerprint` out of `source_home` into the checkout's keyring
/// (created as `<package>.keyring` when there is none, extended otherwise). The
/// key must be the only one exported for that fingerprint, and every signature
/// of a plain archive in the checkout must verify with that key alone.
pub(crate) fn accept_key_files(
    checkout: &Path,
    fingerprint: &str,
    source_home: &Path,
    gpg: &Path,
) -> Result<Accepted, String> {
    let fpr = parse_fingerprint(fingerprint)?;
    let keyrings: Vec<String> = file_names(checkout)
        .into_iter()
        .filter(|n| n.ends_with(".keyring"))
        .collect();
    if keyrings.len() > 1 {
        return Err(format!(
            "the checkout has {} keyrings ({}); a maintainer must remove the stale ones",
            keyrings.len(),
            keyrings.join(", ")
        ));
    }
    let keyring = keyrings
        .first()
        .cloned()
        .unwrap_or_else(|| format!("{}.keyring", name_of(checkout)));
    let path = checkout.join(&keyring);
    let previous = match fs::read_to_string(&path) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("{keyring} cannot be read: {e}")),
    };

    let scratch = Home::new()?;
    let blob = export_key(gpg, source_home, &fpr)?;
    let blob_path: PathBuf = scratch.path().join("new-key.asc");
    fs::write(&blob_path, &blob).map_err(|e| format!("cannot write the key: {e}"))?;
    let shown = primary_fingerprints(&colon_output(
        gpg,
        scratch.path(),
        &["--show-keys"],
        &blob_path,
    )?);
    if shown != [fpr.clone()] {
        return Err(format!(
            "the key exported for {fpr} holds {}, not that key alone; nothing is accepted",
            if shown.is_empty() {
                "no primary key".to_string()
            } else {
                shown.join(", ")
            }
        ));
    }

    // The candidate home holds the keyring as it is and the new key, nothing else.
    let candidate = Home::new()?;
    if previous.is_some() {
        import(gpg, candidate.path(), &path)?;
    }
    let held = primary_fingerprints(&colon_output(
        gpg,
        candidate.path(),
        &["--list-keys"],
        Path::new(""),
    )?);
    if held.contains(&fpr) {
        return Err(format!("{fpr} is already in {keyring}"));
    }
    import(gpg, candidate.path(), &blob_path)?;
    verify_signatures(gpg, candidate.path(), checkout, &fpr)?;

    let mut text = previous.clone().unwrap_or_default();
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(&blob);
    fsx::write_atomic(&path, text).map_err(|e| format!("cannot write {keyring}: {e}"))?;
    Ok(Accepted {
        fingerprint: fpr,
        keyring,
        previous,
    })
}

/// The armored public key of `fpr` in `source_home`, the one key gpg exports for
/// that fingerprint. Empty output is refused.
fn export_key(gpg: &Path, source_home: &Path, fpr: &str) -> Result<String, String> {
    let out = gpg_command(gpg, source_home)
        .args(["--armor", "--export"])
        .arg(fpr)
        .output()
        .map_err(|e| format!("gpg cannot run ({e})"))?;
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    if !out.status.success() || !text.contains("BEGIN PGP PUBLIC KEY BLOCK") {
        return Err(format!(
            "no key {fpr} in the keys looked up ({})",
            first_line(&out.stderr)
        ));
    }
    Ok(text)
}

/// `gpg --import` of `file` into `home`.
fn import(gpg: &Path, home: &Path, file: &Path) -> Result<(), String> {
    let out = gpg_command(gpg, home)
        .arg("--import")
        .arg(file)
        .output()
        .map_err(|e| format!("gpg cannot run ({e})"))?;
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "{} cannot be read by gpg ({}); nothing is accepted",
            file.display(),
            first_line(&out.stderr)
        ))
    }
}

/// The `--with-colons` output of `gpg args` in `home`, for `file` when given.
fn colon_output(gpg: &Path, home: &Path, args: &[&str], file: &Path) -> Result<String, String> {
    let mut cmd = gpg_command(gpg, home);
    cmd.arg("--with-colons").args(args);
    if !file.as_os_str().is_empty() {
        cmd.arg(file);
    }
    let out = cmd.output().map_err(|e| format!("gpg cannot run ({e})"))?;
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The primary fingerprints of the keys in `--with-colons` output: the `fpr`
/// line that follows each `pub` line.
fn primary_fingerprints(colons: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut want = false;
    for line in colons.lines() {
        if line.starts_with("pub:") {
            want = true;
        } else if want && line.starts_with("fpr:") {
            out.push(line.split(':').nth(9).unwrap_or("").to_ascii_uppercase());
            want = false;
        }
    }
    out
}

/// Every detached signature in the checkout whose plain archive is present
/// must verify in `home` with a signature by `fpr` (its primary key).
fn verify_signatures(gpg: &Path, home: &Path, checkout: &Path, fpr: &str) -> Result<(), String> {
    let pairs: Vec<(String, String)> = file_names(checkout)
        .into_iter()
        .filter(|n| SIGNATURE_SUFFIXES.iter().any(|s| n.ends_with(s)))
        .filter_map(|sig| {
            let archive = archive_of(&sig);
            checkout.join(&archive).is_file().then_some((sig, archive))
        })
        .collect();
    if pairs.is_empty() {
        return Err(
            "no signature of a plain archive is in the checkout to check the key against; \
             nothing is accepted"
                .into(),
        );
    }
    for (sig, archive) in pairs {
        let out = gpg_command(gpg, home)
            .args(["--status-fd", "1", "--verify"])
            .arg(checkout.join(&sig))
            .arg(checkout.join(&archive))
            .output()
            .map_err(|e| format!("gpg cannot run ({e})"))?;
        let status = String::from_utf8_lossy(&out.stdout);
        if status.contains("[GNUPG:] REVKEYSIG ") {
            return Err(format!(
                "{sig}: the key {fpr} is revoked; nothing is accepted"
            ));
        }
        // VALIDSIG: `fpr date ts exp ver res pkalgo hashalgo class primary-fpr`.
        let signer = status
            .lines()
            .find_map(|l| l.strip_prefix("[GNUPG:] VALIDSIG "))
            .and_then(|rest| rest.split_whitespace().nth(9));
        match signer {
            None => {
                return Err(format!(
                    "{sig} does not verify {archive} with the keys in the keyring and {fpr}; \
                     nothing is accepted"
                ));
            }
            Some(s) if !s.eq_ignore_ascii_case(fpr) => {
                return Err(format!(
                    "{sig} is signed by key {s}, not by {fpr}; nothing is accepted"
                ));
            }
            Some(_) => {}
        }
    }
    Ok(())
}

fn name_of(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// A GnuPG home of its own, removed with its agents stopped.
pub(super) struct Home(tempfile::TempDir);

impl Home {
    pub(super) fn new() -> Result<Self, String> {
        tempfile::Builder::new()
            .prefix("auto-package-gpg")
            .tempdir()
            .map(Home)
            .map_err(|e| format!("no temporary GnuPG home ({e})"))
    }

    pub(super) fn path(&self) -> &Path {
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
