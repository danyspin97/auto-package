//! What a signature refusal says a keyring holds: its primary keys, their user
//! IDs and their state, read from `gpg --with-colons --list-keys`. Names come
//! from the keyring file, so they are printed only after their control and
//! bidi-override characters are removed.

use super::source_signature::gpg_command;
use std::path::Path;

/// One primary key of the keyring.
#[derive(Default)]
struct HeldKey {
    fpr: String,
    /// `expired` or `revoked` (the key's validity), `""` for a valid key.
    state: &'static str,
    /// The expiry date as `YYYY-MM-DD`, when gpg gives one (colon field 7).
    expires: Option<String>,
    /// Each user ID with whether it is revoked.
    uids: Vec<(String, bool)>,
}

/// What the keyring imported into `home` holds (see [`parse_holds`]).
pub(super) fn keyring_holds(gpg: &Path, home: &Path) -> String {
    match gpg_command(gpg, home)
        .args(["--with-colons", "--list-keys"])
        .output()
    {
        Ok(out) => parse_holds(&String::from_utf8_lossy(&out.stdout)),
        Err(_) => "its keys cannot be listed".to_string(),
    }
}

/// The keys of a `gpg --with-colons --list-keys` listing, one entry per primary
/// key: its fingerprint, its user IDs and its state.
pub(super) fn parse_holds(colons: &str) -> String {
    let mut keys: Vec<HeldKey> = Vec::new();
    for line in colons.lines() {
        let fields: Vec<&str> = line.split(':').collect();
        match fields.first().copied() {
            Some("pub") => keys.push(HeldKey {
                state: match fields.get(1).copied() {
                    Some("e") => "expired",
                    Some("r") => "revoked",
                    _ => "",
                },
                expires: fields.get(6).and_then(|e| date_of_epoch(e)),
                ..Default::default()
            }),
            Some("fpr") => {
                if let Some(key) = keys.last_mut()
                    && key.fpr.is_empty()
                    && let Some(fpr) = fields.get(9)
                {
                    key.fpr = fpr.to_string();
                }
            }
            Some("uid") => {
                if let Some(key) = keys.last_mut()
                    && let Some(uid) = fields.get(9)
                    && !uid.is_empty()
                {
                    key.uids.push((printable(uid), fields.get(1) == Some(&"r")));
                }
            }
            _ => {}
        }
    }
    if keys.is_empty() {
        return "no key".to_string();
    }
    keys.iter().map(describe).collect::<Vec<_>>().join("; ")
}

fn describe(key: &HeldKey) -> String {
    let who = if key.uids.is_empty() {
        "no user ID".to_string()
    } else {
        key.uids
            .iter()
            .map(|(name, revoked)| {
                if *revoked {
                    format!("{name} (revoked)")
                } else {
                    name.clone()
                }
            })
            .collect::<Vec<_>>()
            .join(", ")
    };
    let when = match (key.state, &key.expires) {
        ("revoked", _) => ", revoked".to_string(),
        ("expired", Some(date)) => format!(", expired on {date}"),
        ("expired", None) => ", expired".to_string(),
        ("", Some(date)) => format!(", expires {date}"),
        _ => String::new(),
    };
    format!("{} ({who}{when})", key.fpr)
}

/// The most characters of one user ID a refusal prints (gpg accepts 64 KB).
const MAX_UID_CHARS: usize = 80;

/// A user ID as a refusal prints it: gpg's `\xHH` escapes decoded, then every
/// invisible or direction-changing character removed, the characters the
/// listing itself uses (`;`, `,`, `(`, `)`) and the ones that open a code span
/// or a string (backtick, `"`) replaced, and the name cut to
/// [`MAX_UID_CHARS`] characters with `...`, so a name cannot hide text, turn
/// the line around, forge another key's entry or fill the refusal.
pub(super) fn printable(raw: &str) -> String {
    let mut chars: Vec<char> = String::from_utf8_lossy(&unescape(raw))
        .chars()
        .filter(|c| !is_hidden(*c))
        .map(|c| match c {
            ';' | ',' | '(' | ')' | '`' | '"' => '_',
            c => c,
        })
        .collect();
    if chars.len() > MAX_UID_CHARS {
        chars.truncate(MAX_UID_CHARS);
        chars.extend("...".chars());
    }
    chars.into_iter().collect()
}

/// Whether `c` is invisible or changes the direction of the text around it:
/// controls, the soft hyphen, the combining grapheme joiner, the Hangul and
/// Braille fillers, bidi embeddings, overrides and isolates, the line and
/// paragraph separators, the marks, zero-width characters and invisible
/// operators, the Mongolian and variation selectors, the interlinear
/// annotation marks, the object replacement characters, the byte order mark
/// and the tag block.
pub(crate) fn is_hidden(c: char) -> bool {
    c.is_control()
        || is_bidi_override(c)
        || matches!(
            c,
            '\u{00AD}'
                | '\u{034F}'
                | '\u{061C}'
                | '\u{115F}'
                | '\u{1160}'
                | '\u{180B}'..='\u{180E}'
                | '\u{200B}'..='\u{200F}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{2060}'..='\u{206F}'
                | '\u{2800}'
                | '\u{3164}'
                | '\u{FE00}'..='\u{FE0F}'
                | '\u{FEFF}'
                | '\u{FFA0}'
                | '\u{FFF9}'..='\u{FFFC}'
                | '\u{E0000}'..='\u{E007F}'
                | '\u{E0100}'..='\u{E01EF}'
        )
}

/// Whether `c` is a bidirectional embedding, override or isolate control.
pub(super) fn is_bidi_override(c: char) -> bool {
    matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

/// The bytes of `raw` with each `\xHH` (gpg escapes `:` and unprintable bytes
/// this way) replaced by the byte it names.
fn unescape(raw: &str) -> Vec<u8> {
    let b = raw.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\'
            && b.get(i + 1) == Some(&b'x')
            && let Some(hex) = raw.get(i + 2..i + 4)
            && hex.chars().all(|c| c.is_ascii_hexdigit())
            && let Ok(byte) = u8::from_str_radix(hex, 16)
        {
            out.push(byte);
            i += 4;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    out
}

/// The `YYYY-MM-DD` of an epoch-seconds field, `None` when it is empty or not a
/// number.
fn date_of_epoch(field: &str) -> Option<String> {
    let secs: i64 = field.parse().ok()?;
    let (y, m, d) = civil_from_days(secs.div_euclid(86_400));
    Some(format!("{y:04}-{m:02}-{d:02}"))
}

/// The proleptic Gregorian date of `days` since 1970-01-01.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}
