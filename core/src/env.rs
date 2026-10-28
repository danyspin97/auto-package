//! Reading on/off switches from the environment.

/// True only for `1`, `true` or `yes` (any case); unset, empty, `0` and anything else is false.
pub fn env_flag(name: &str) -> bool {
    std::env::var_os(name)
        .and_then(|v| v.into_string().ok())
        .is_some_and(|v| flag_value(&v))
}

/// The value test behind [`env_flag`].
pub fn flag_value(v: &str) -> bool {
    let v = v.trim();
    v == "1" || v.eq_ignore_ascii_case("true") || v.eq_ignore_ascii_case("yes")
}

/// A positive, finite number from the environment. Unset, empty, unparsable,
/// zero, negative or non-finite values give `None`, so the caller's default applies.
pub fn env_positive_f64(name: &str) -> Option<f64> {
    positive_f64(&std::env::var(name).ok()?)
}

/// The value test behind [`env_positive_f64`].
pub fn positive_f64(v: &str) -> Option<f64> {
    v.trim()
        .parse::<f64>()
        .ok()
        .filter(|n| n.is_finite() && *n > 0.0)
}

/// The process environment as strings. Entries whose name or value is not valid
/// UTF-8 are skipped (`std::env::vars` panics on them).
pub fn utf8_vars() -> Vec<(String, String)> {
    utf8_pairs(std::env::vars_os())
}

/// Keep the pairs that are valid UTF-8 on both sides.
pub fn utf8_pairs(
    vars: impl IntoIterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
) -> Vec<(String, String)> {
    vars.into_iter()
        .filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_affirmative_values_enable() {
        for v in ["1", "true", "TRUE", "Yes", "yes"] {
            assert!(flag_value(v), "{v}");
        }
        for v in ["", "0", "false", "no", "2", "off", "enabled"] {
            assert!(!flag_value(v), "{v}");
        }
    }

    #[test]
    fn positive_numbers_only() {
        assert_eq!(positive_f64(" 2.5 "), Some(2.5));
        for v in ["", "0", "-1", "abc", "NaN", "inf"] {
            assert_eq!(positive_f64(v), None, "{v}");
        }
    }

    #[test]
    fn unset_is_false() {
        assert!(!env_flag("AUTO_PACKAGE_TEST_SURELY_UNSET_FLAG"));
    }

    #[test]
    fn non_utf8_entries_are_skipped() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;
        let bad = OsString::from_vec(vec![b'x', 0xff, 0xfe]);
        let pairs = utf8_pairs([
            (OsString::from("A"), OsString::from("1")),
            (OsString::from("B"), bad.clone()),
            (bad, OsString::from("2")),
        ]);
        assert_eq!(pairs, vec![("A".to_string(), "1".to_string())]);
    }
}
