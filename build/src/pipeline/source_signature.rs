//! The signatures of the new source files, checked before `osc ci`.
//!
//! OBS's source validator (`25-keyring-validate`, run by `osc ci`) verifies
//! each detached signature in the checkout against the package's keyring: a
//! signed archive that the keyring cannot verify fails the commit with no hint
//! of why. This applies the same rules offline first, in a fresh GnuPG home
//! that holds only that keyring. A signature whose archive is only present
//! compressed (`X.gz`, `X.bz2`, `X.xz`, `X.zst` for a signature of `X`) is
//! verified over the decompressed stream. No key is trusted here: a key the
//! keyring lacks is a maintainer's decision.

use super::keyring_accept::accept_command;
use super::keyring_listing::keyring_holds;
use std::path::Path;
use std::process::{Command, Stdio};

/// The detached-signature suffixes, stripped in the order the validator strips them.
pub(super) const SIGNATURE_SUFFIXES: &[&str] = &[".asc", ".sig", ".sign"];

/// The compressed forms an archive may have, with the tool that decompresses each.
const COMPRESSED: &[(&str, &str)] = &[
    ("gz", "gzip"),
    ("bz2", "bzip2"),
    ("xz", "xz"),
    ("zst", "zstd"),
];

#[derive(Debug, PartialEq, Eq)]
pub(super) enum SignatureCheck {
    /// Nothing is signed, or every signed archive verifies.
    Ok,
    /// The check could not run here; the text is the note to show. The
    /// commit's own validator decides.
    Skipped(String),
    /// A new archive does not verify. Nothing may go to OBS until a maintainer
    /// has decided.
    Refused(String),
}

/// Checks the signed files of `checkout` with `gpg` (the program, looked up on
/// `PATH` when it is a bare name).
pub(super) fn check_signed_sources(checkout: &Path) -> SignatureCheck {
    check_with(Path::new("gpg"), checkout)
}

/// The keyring of the checkout: its file name and what it holds, for a refusal.
pub(super) struct Keyring<'a> {
    file: &'a str,
    holds: String,
}

pub(super) fn check_with(gpg: &Path, checkout: &Path) -> SignatureCheck {
    let names = file_names(checkout);
    let signatures: Vec<&String> = names
        .iter()
        .filter(|n| SIGNATURE_SUFFIXES.iter().any(|s| n.ends_with(s)))
        .collect();
    if signatures.is_empty() {
        return SignatureCheck::Ok;
    }
    if names.iter().any(|n| n.ends_with(".minisig")) {
        return SignatureCheck::Skipped(
            "the checkout has minisign signatures, which are not checked here".into(),
        );
    }
    // The validator expects exactly one keyring; a stale one is for a maintainer.
    let keyrings: Vec<&String> = names.iter().filter(|n| n.ends_with(".keyring")).collect();
    if keyrings.len() > 1 {
        let all = keyrings
            .iter()
            .map(|k| k.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        return SignatureCheck::Refused(format!(
            "the checkout has {} keyrings ({all}), and OBS's source validator fails on more \
             than one; a maintainer must remove the stale ones. Nothing was sent to OBS",
            keyrings.len()
        ));
    }
    let Some(file) = keyrings.first() else {
        return SignatureCheck::Skipped(format!(
            "{} is signed, but the checkout has no keyring, so its signature is not checked here",
            signatures[0]
        ));
    };
    // A fresh home: only the package's keyring is ever imported into it, and
    // the user's own GnuPG home is never named.
    let home = match tempfile::Builder::new()
        .prefix("auto-package-gpg")
        .tempdir()
    {
        Ok(home) => home,
        Err(e) => {
            return SignatureCheck::Skipped(format!(
                "no temporary GnuPG home ({e}), so the signature of {} is not checked here",
                signatures[0]
            ));
        }
    };
    let home = home.path();
    if gpg_command(gpg, home).arg("--version").output().is_err() {
        return SignatureCheck::Skipped(format!(
            "gpg is not installed, so the signature of {} is not checked here",
            signatures[0]
        ));
    }
    match gpg_command(gpg, home)
        .arg("--import")
        .arg(checkout.join(file))
        .output()
    {
        Ok(o) if o.status.success() => {}
        Ok(o) => {
            return SignatureCheck::Refused(format!(
                "the keyring {file} cannot be read by gpg ({}), so no signature is \
                 checked against it; a maintainer must look at it. Nothing was sent to OBS",
                first_line(&o.stderr)
            ));
        }
        Err(e) => {
            return SignatureCheck::Skipped(format!(
                "gpg cannot run ({e}), so the signature of {} is not checked here",
                signatures[0]
            ));
        }
    }
    let keyring = Keyring {
        file: file.as_str(),
        holds: keyring_holds(gpg, home),
    };
    for sig in signatures {
        match check_signature(gpg, home, checkout, sig, &keyring) {
            Ok(None) => {}
            Ok(Some(refusal)) => return SignatureCheck::Refused(refusal),
            Err(note) => return SignatureCheck::Skipped(note),
        }
    }
    SignatureCheck::Ok
}

/// Checks the archive of the signature `sig`: the file it names when that is
/// present, else each compressed form of it that is. `Ok(Some(why))` refuses,
/// `Err(note)` is a check that could not run here.
fn check_signature(
    gpg: &Path,
    home: &Path,
    checkout: &Path,
    sig: &str,
    keyring: &Keyring,
) -> Result<Option<String>, String> {
    let archive = archive_of(sig);
    if checkout.join(&archive).is_file() {
        return verify_file(gpg, home, checkout, sig, &archive, keyring);
    }
    for (ext, tool) in COMPRESSED {
        let compressed = format!("{archive}.{ext}");
        if checkout.join(&compressed).is_file()
            && let Some(why) =
                verify_compressed(gpg, home, checkout, sig, &compressed, tool, keyring)?
        {
            return Ok(Some(why));
        }
    }
    Ok(None)
}

/// `gpg --verify` of `sig` over the file `archive`.
fn verify_file(
    gpg: &Path,
    home: &Path,
    checkout: &Path,
    sig: &str,
    archive: &str,
    keyring: &Keyring,
) -> Result<Option<String>, String> {
    let out = gpg_command(gpg, home)
        .args(["--status-fd", "1", "--verify"])
        .arg(checkout.join(sig))
        .arg(checkout.join(archive))
        .output()
        .map_err(|e| {
            format!("gpg cannot run ({e}), so the signature of {archive} is not checked here")
        })?;
    let status = String::from_utf8_lossy(&out.stdout);
    Ok(refusal(checkout, archive, sig, keyring, &status, &out))
}

/// `gpg --verify` of `sig` over the decompressed stream of `compressed`, piped
/// from the decompressor so that no whole archive is held in memory.
fn verify_compressed(
    gpg: &Path,
    home: &Path,
    checkout: &Path,
    sig: &str,
    compressed: &str,
    tool: &str,
    keyring: &Keyring,
) -> Result<Option<String>, String> {
    let not_run = |e: std::io::Error| {
        format!("{tool} cannot run ({e}), so the signature of {compressed} is not checked here")
    };
    let mut decompressor = Command::new(tool)
        .arg("-dc")
        .arg(checkout.join(compressed))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(not_run)?;
    let Some(stream) = decompressor.stdout.take() else {
        return Err(format!(
            "{tool} gave no output pipe, so {compressed} is not checked here"
        ));
    };
    let verdict = gpg_command(gpg, home)
        .args(["--status-fd", "1", "--verify"])
        .arg(checkout.join(sig))
        .arg("-")
        .stdin(Stdio::from(stream))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output();
    let finished = decompressor.wait_with_output();
    let gpg_out = verdict.map_err(|e| {
        format!("gpg cannot run ({e}), so the signature of {compressed} is not checked here")
    })?;
    let decompressed = finished.map_err(not_run)?;
    if !decompressed.status.success() {
        return Ok(Some(format!(
            "the new {compressed} cannot be decompressed by {tool} ({}), so it is not checked \
             against {sig}; a maintainer must check it. Nothing was sent to OBS",
            first_line(&decompressed.stderr)
        )));
    }
    let status = String::from_utf8_lossy(&gpg_out.stdout);
    Ok(refusal(
        checkout, compressed, sig, keyring, &status, &gpg_out,
    ))
}

/// `gpg` run in `home` only: `--homedir` and `GNUPGHOME` both name it, no agent
/// is started, and no key is fetched from a keyserver or located by its owner.
pub(super) fn gpg_command(gpg: &Path, home: &Path) -> Command {
    let mut cmd = Command::new(gpg);
    cmd.arg("--homedir")
        .arg(home)
        .args([
            "--batch",
            "--no-autostart",
            "--no-tty",
            "--no-auto-key-retrieve",
            "--no-auto-key-locate",
        ])
        .env("GNUPGHOME", home)
        .stdin(Stdio::null());
    cmd
}

/// Why the verdict on `archive` refuses it, `None` when it verifies. An expired
/// key verifies here (`EXPKEYSIG` with `VALIDSIG`, exit 0), as OBS's validator
/// accepts it; a revoked key does not (`REVKEYSIG`, which gpg also reports with
/// exit 0).
fn refusal(
    checkout: &Path,
    archive: &str,
    sig: &str,
    keyring: &Keyring,
    status: &str,
    out: &std::process::Output,
) -> Option<String> {
    let line = |tag: &str| {
        status
            .lines()
            .find(|l| l.starts_with(&format!("[GNUPG:] {tag} ")))
    };
    // VALIDSIG ends with the fingerprint of the signing key in its third field.
    let signer = line("VALIDSIG")
        .and_then(|l| l.split_whitespace().nth(2))
        .unwrap_or("unknown");
    if line("REVKEYSIG").is_some() {
        return Some(format!(
            "the new {archive} is signed with key {signer}, which its owner has revoked; a \
             maintainer must decide. Nothing was sent to OBS"
        ));
    }
    if out.status.success() && line("VALIDSIG").is_some() {
        return None;
    }
    if let Some(err) = line("ERRSIG") {
        // ERRSIG: `[GNUPG:] ERRSIG keyid pkalgo hashalgo class time rc [fpr]`.
        let fields: Vec<&str> = err.split_whitespace().collect();
        let rc = fields.get(7).copied().unwrap_or("");
        let fpr = fields
            .get(8)
            .filter(|f| f.len() == 40 && f.chars().all(|c| c.is_ascii_hexdigit()))
            .or_else(|| fields.get(2))
            .copied()
            .unwrap_or("unknown");
        return Some(match rc {
            // 9: the key is not on the keyring.
            "9" => not_in_keyring(checkout, archive, sig, fpr, keyring),
            // 4: an algorithm this gpg does not implement.
            "4" => format!(
                "gpg does not support the signature algorithm of key {fpr}, so the new {archive} \
                 cannot be checked against {sig}: verify it by hand. Nothing was sent to OBS"
            ),
            _ => format!(
                "gpg could not verify the new {archive} against {sig} (signature status {rc}); a \
                 maintainer must check it. Nothing was sent to OBS"
            ),
        });
    }
    if let Some(key) = line("NO_PUBKEY") {
        // NO_PUBKEY has only the key id.
        let id = key.split_whitespace().nth(2).unwrap_or("unknown");
        return Some(not_in_keyring(checkout, archive, sig, id, keyring));
    }
    if line("BADSIG").is_some() {
        return Some(format!(
            "the new {archive} does not verify against {sig} with {}: the archive or its \
             signature is not what the key signed; a maintainer must check it. Nothing was sent \
             to OBS",
            keyring.file
        ));
    }
    Some(format!(
        "gpg could not verify the new {archive} against {sig} ({}); a maintainer must check it. \
         Nothing was sent to OBS",
        first_line(&out.stderr)
    ))
}

/// The refusal for a signature by a key the keyring lacks. `key` is the full
/// fingerprint when gpg has it (the signature's issuer fingerprint), else only
/// the key ID. The text says what the keyring holds, where to check the
/// fingerprint, and the command that accepts the key once a maintainer has
/// checked it on a second channel.
fn not_in_keyring(
    checkout: &Path,
    archive: &str,
    sig: &str,
    key: &str,
    keyring: &Keyring,
) -> String {
    let full = key.len() == 40 && key.chars().all(|c| c.is_ascii_hexdigit());
    let (named, shown) = if full {
        (
            format!(
                "key {key}, from the issuer fingerprint of {sig}; the signature carries no user \
                 ID, so none is known here"
            ),
            key,
        )
    } else {
        (
            format!(
                "key ID {key}, named by {sig}; the signature gives no full fingerprint, so look \
                 the 40-digit fingerprint up on the upstream's site"
            ),
            "<FULL-FINGERPRINT>",
        )
    };
    format!(
        "the new {archive} is signed with {named}, which is not in {file} (it holds: {holds}). \
         Check the fingerprint against upstream's release announcement or website; a maintainer \
         must decide, then run `{cmd}`. The tool never trusts a key by itself. Nothing was sent \
         to OBS",
        file = keyring.file,
        holds = keyring.holds,
        cmd = accept_command(checkout, shown)
    )
}

/// The archive a signature file names: its name without the signature suffix
/// (the validator strips them in turn, `.asc` first).
pub(super) fn archive_of(sig: &str) -> String {
    let mut name = sig;
    for suffix in SIGNATURE_SUFFIXES {
        name = name.strip_suffix(suffix).unwrap_or(name);
    }
    name.to_string()
}

/// The names of the regular files directly in `checkout`, sorted. A name that
/// starts with a dot is not matched, as the validator's shell globs do not.
pub(super) fn file_names(checkout: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(checkout)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_file())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| !n.starts_with('.'))
        .collect();
    names.sort();
    names
}

pub(super) fn first_line(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("no output")
        .trim()
        .to_string()
}
