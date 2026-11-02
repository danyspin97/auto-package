//! The RPM `Version:` of an upstream tag that carries a dash (`v2.9.0-1`,
//! `0.1.0-alpha`), see [`rpm_version_for_tag`].

/// The RPM `Version:` of an upstream tag that carries a dash, once its prefix
/// (`v`, `r`, `release-`) is off. An RPM `Version:` cannot hold a dash, so the
/// part after the last `-` decides:
///
/// - digits under 1000 after `X.Y.Z` are a revision counter: `v2.9.0-1` is
///   `2.9.0.1` (the form Louvre and SRM are packaged under). A two-part
///   `1.0-2` is refused, since `1.0.2` is a release of its own, and so is a
///   counter of four digits or more (`1.0-2024`), a date or a build number;
/// - `alpha`, `beta`, `rc` or `pre`, in any case, with digits or a dot and
///   digits after it: `0.1.0-alpha` is `0.1.0~alpha`, `1.2.3-rc.1` is
///   `1.2.3~rc1`;
/// - anything else (`1.2.3-r1` is a revision spelling, not a pre-release), or
///   a part before the dash that is not dotted numbers, is refused: a
///   maintainer has to say what the tag means.
///
/// A tag without a dash is its version as it is.
pub fn rpm_version_for_tag(tag: &str) -> Result<String, String> {
    let version = crate::version::strip_prefix(tag.trim());
    let Some((head, tail)) = version.rsplit_once('-') else {
        return Ok(version.to_string());
    };
    let refuse = || {
        format!(
            "upstream tag {tag} needs a maintainer: a dash in it is neither a revision \
             counter nor a pre-release tag"
        )
    };
    let dotted = head
        .split('.')
        .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()));
    if !dotted {
        return Err(refuse());
    }
    // Under four digits, after X.Y.Z: `1.0-2` would be the release `1.0.2`,
    // and `0.19-20260627` or `1.0-2024` are dates or build numbers.
    let digits = !tail.is_empty() && tail.bytes().all(|b| b.is_ascii_digit());
    if digits && head.split('.').count() >= 3 && tail.parse::<u64>().is_ok_and(|n| n < 1000) {
        return Ok(format!("{head}.{tail}"));
    }
    let lower = tail.to_ascii_lowercase();
    let word_end = lower
        .find(|c: char| c.is_ascii_digit() || c == '.')
        .unwrap_or(lower.len());
    let (word, rest) = lower.split_at(word_end);
    let shape_ok = match rest.strip_prefix('.') {
        Some(n) => !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()),
        None => rest.bytes().all(|b| b.is_ascii_digit()),
    };
    if matches!(word, "alpha" | "beta" | "rc" | "pre") && shape_ok {
        let number = rest.trim_start_matches('.');
        return Ok(format!("{head}~{word}{number}"));
    }
    Err(refuse())
}

#[cfg(test)]
mod tests {
    use super::rpm_version_for_tag;
    use crate::version::{Scheme, cmp};
    use std::cmp::Ordering::{Greater, Less};

    #[test]
    fn a_revision_counter_is_a_fourth_component() {
        // Louvre and SRM tag their revisions with a dash.
        assert_eq!(rpm_version_for_tag("v2.8.0-1").as_deref(), Ok("2.8.0.1"));
        assert_eq!(rpm_version_for_tag("v2.9.0-1").as_deref(), Ok("2.9.0.1"));
        assert_eq!(rpm_version_for_tag("v0.8.0-1").as_deref(), Ok("0.8.0.1"));
        assert_eq!(rpm_version_for_tag("v0.8.0-2").as_deref(), Ok("0.8.0.2"));
        assert_eq!(
            rpm_version_for_tag("release-1.0.0-12").as_deref(),
            Ok("1.0.0.12")
        );
    }

    #[test]
    fn a_pre_release_is_a_tilde_version() {
        assert_eq!(
            rpm_version_for_tag("0.1.0-alpha").as_deref(),
            Ok("0.1.0~alpha")
        );
        assert_eq!(
            rpm_version_for_tag("1.3.12-pre3").as_deref(),
            Ok("1.3.12~pre3")
        );
        assert_eq!(rpm_version_for_tag("2.0-RC1").as_deref(), Ok("2.0~rc1"));
        assert_eq!(rpm_version_for_tag("2.0-Beta").as_deref(), Ok("2.0~beta"));
        assert_eq!(
            rpm_version_for_tag("1.2.3-rc.1").as_deref(),
            Ok("1.2.3~rc1")
        );
        assert_eq!(
            rpm_version_for_tag("1.0.0-beta.2").as_deref(),
            Ok("1.0.0~beta2")
        );
    }

    #[test]
    fn a_two_part_or_long_counter_is_refused() {
        // `1.0-2` as `1.0.2` would be a release of its own.
        for tag in [
            "1.0-2",
            "v2.9-1",
            "1.2.3-1000",
            "1.2.3-2024",
            "1.2.3-20260627",
        ] {
            let err = rpm_version_for_tag(tag).unwrap_err();
            assert!(err.contains("needs a maintainer"), "{err}");
        }
        assert_eq!(rpm_version_for_tag("1.2.3-999").as_deref(), Ok("1.2.3.999"));
    }

    #[test]
    fn a_tag_without_a_dash_is_its_version() {
        assert_eq!(rpm_version_for_tag("v1.2").as_deref(), Ok("1.2"));
        assert_eq!(rpm_version_for_tag("r62").as_deref(), Ok("62"));
    }

    #[test]
    fn anything_else_with_a_dash_needs_a_maintainer() {
        for tag in [
            "0.19-20260627",
            "2020-11-13",
            "25.12.19-2026-02-06-03fffb2",
            "1.0-prefix",
            "1.0-stable",
            "1.2.3-r1",
            "1.2.3-rc.1.2",
            "1.2.3-rc.",
            "louvre-2.9.0-1",
            "1.0-",
            "-1",
        ] {
            let err = rpm_version_for_tag(tag).unwrap_err();
            assert!(
                err.contains(tag) && err.contains("needs a maintainer"),
                "{err}"
            );
        }
    }

    #[test]
    fn mapped_revisions_sort_as_releases() {
        let a = rpm_version_for_tag("v0.8.0-2").unwrap();
        let b = rpm_version_for_tag("v0.8.0-1").unwrap();
        assert_eq!(cmp(&a, &b, Scheme::Generic), Some(Greater));
        assert_eq!(cmp(&b, "0.8.0", Scheme::Generic), Some(Greater));
        let pre = rpm_version_for_tag("0.1.0-alpha").unwrap();
        assert_eq!(cmp(&pre, "0.1.0", Scheme::Generic), Some(Less));
    }
}
