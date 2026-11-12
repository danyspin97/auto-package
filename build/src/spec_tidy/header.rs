//! The licence header every spec starts with. One function renders it for
//! every generator, and a tidy pass adds it to a spec that has none.

use std::cell::Cell;

/// The holder the generated header names.
pub const HOLDER: &str = "SUSE LLC and contributors";

thread_local! {
    static FIXED_YEAR: Cell<Option<i64>> = const { Cell::new(None) };
}

/// The current UTC year. A test pins it with [`with_year`].
pub fn current_year() -> i64 {
    if let Some(year) = FIXED_YEAR.with(Cell::get) {
        return year;
    }
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    year_of_epoch(secs)
}

/// The UTC year of a Unix time.
pub fn year_of_epoch(secs: i64) -> i64 {
    crate::osc::utc_stamp(secs)
        .split('-')
        .next()
        .and_then(|y| y.parse().ok())
        .unwrap_or(1970)
}

/// Run `f` with [`current_year`] fixed to `year` on this thread. The clock
/// is restored even when `f` panics.
pub fn with_year<T>(year: i64, f: impl FnOnce() -> T) -> T {
    struct Restore(Option<i64>);
    impl Drop for Restore {
        fn drop(&mut self) {
            FIXED_YEAR.with(|c| c.set(self.0));
        }
    }
    let _restore = Restore(FIXED_YEAR.with(|c| c.replace(Some(year))));
    f()
}

/// The header of the spec of package `package`, ending in the blank lines
/// that separate it from the first tag.
pub fn spec_header(package: &str, year: i64) -> String {
    format!(
        "#\n\
         # spec file for package {package}\n\
         #\n\
         # Copyright (c) {year} {HOLDER}\n\
         #\n\
         # All modifications and additions to the file contributed by third parties\n\
         # remain the property of their copyright owners, unless otherwise agreed\n\
         # upon. The license for this file, and modifications and additions to the\n\
         # file, is the same license as for the pristine package itself (unless the\n\
         # license for the pristine package is not an Open Source License, in which\n\
         # case the license is the MIT License). An \"Open Source License\" is a\n\
         # license that conforms to the Open Source Definition (Version 1.9)\n\
         # published by the Open Source Initiative.\n\
         \n\
         # Please submit bugfixes or comments via https://bugs.opensuse.org/\n\
         #\n\
         \n\
         \n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_year_follows_the_clock() {
        let out = with_year(2044, || spec_header("x", current_year()));
        assert!(out.contains("Copyright (c) 2044 SUSE LLC and contributors\n"));
        assert!(out.contains("bugs.opensuse.org/\n#\n\n\n"));
        assert!(!out.contains("2026"));
        assert_eq!(year_of_epoch(1_798_000_000), 2026);
        assert_eq!(year_of_epoch(1_830_297_600), 2028);
    }
}
