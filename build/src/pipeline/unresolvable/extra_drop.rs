//! A requirement OBS reports missing only because the spec asks for an extra
//! of it: `nothing provides python313-setuptools_scm[toml]` while
//! `python313-setuptools_scm` is published. The package exists, so nothing is
//! built; the spec drops the extra, which no distro package provides.

/// The distro package and the extras a `nothing provides` line asks for, when
/// the requirement carries extras: `nothing provides python313-x[toml] >= 1`
/// is `("python313-x", "toml")`. `None` for a requirement without extras.
pub(super) fn requested_extra(line: &str) -> Option<(String, String)> {
    let (_, rest) = line.split_once("nothing provides ")?;
    let word = rest.split_whitespace().next()?;
    let (bare, inner) = word.split_once('[')?;
    let extras = inner.strip_suffix(']')?;
    let plain = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | '-'))
    };
    let extras_ok = extras.split(',').all(|e| plain(e.trim()));
    (plain(bare) && extras_ok).then(|| (bare.to_string(), extras.to_string()))
}

/// The published packages whose requested extras are what OBS cannot provide,
/// as `(package, extras)`, each package once. `ships` says whether the distro
/// publishes a package of exactly that name.
pub(super) fn find_extra_drops(
    detail: &[String],
    ships: &dyn Fn(&str) -> bool,
) -> Vec<(String, String)> {
    let mut drops: Vec<(String, String)> = Vec::new();
    for line in detail {
        if let Some((bare, extras)) = requested_extra(line)
            && !drops.iter().any(|(b, _)| *b == bare)
            && ships(&bare)
        {
            drops.push((bare, extras));
        }
    }
    drops
}

/// The note the fix turn gets for `drops`: the extra is dropped from the spec
/// and nothing is built.
pub(super) fn extra_drop_note(drops: &[(String, String)]) -> Option<String> {
    if drops.is_empty() {
        return None;
    }
    let lines: Vec<String> = drops
        .iter()
        .map(|(bare, extras)| {
            format!(
                "{bare} is published, so the missing `[{extras}]` is an extra of it that no \
                 distro package provides. Drop `[{extras}]` from the BuildRequires that asks \
                 for it and keep its version bound. It is not a package to build."
            )
        })
        .collect();
    Some(lines.join("\n"))
}
