//! The single version comparison API (`Scheme`, `cmp`, `strip_prefix`,
//! `is_prerelease`, `is_snapshot`, `leading_numbers`). It lives in
//! `ap_ecosystem::version`, the lowest crate that needs it (the PyPI client and
//! the NVIDIA redist lookup compare versions too), and is re-exported here.

pub use ap_ecosystem::version::*;
