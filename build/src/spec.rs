//! RPM spec generation via py2pack, with post-processing.
//!
//! py2pack handles the openSUSE macro conventions that are otherwise painful
//! to get right. We supply what it can't determine from PyPI alone: the
//! build-backend requirements from the sdist, the correct SPDX license
//! expression, and the upstream URL.

use std::process::{Command, Stdio};

use ap_ecosystem::pypi::{License, Release};
use ap_ecosystem::sdist::BuildSystem;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("could not run py2pack: {0}")]
    Launch(std::io::Error),

    #[error("py2pack exited {code}:\n{stderr}")]
    Failed { code: i32, stderr: String },

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}
mod fixes;
pub use fixes::*;
mod parse;
pub use parse::*;
mod sources;
pub use sources::*;
mod conditions;
pub(crate) use conditions::*;
mod expand;
pub use expand::*;

#[cfg(test)]
mod conditions_tests;

mod build_requires;
mod generate;
#[cfg(test)]
mod tests;
pub use generate::*;
mod skeleton;
pub use skeleton::*;
mod rpmlintrc;
pub use rpmlintrc::*;
mod requires_exclude;
pub use requires_exclude::*;
mod minimums;
pub use minimums::*;
#[cfg(test)]
mod minimums_tests;
mod wheel;
use wheel::*;

impl HeaderCleanup<'_> {
    /// Whether `%install` removes `path` (relative to site-packages).
    fn removes(&self, path: &str) -> bool {
        match self {
            HeaderCleanup::Dir(d) => path == *d || path.starts_with(&format!("{d}/")),
            HeaderCleanup::Headers(d) => {
                path.starts_with(&format!("{d}/"))
                    && (path.ends_with(".h") || path.ends_with(".hpp"))
            }
        }
    }
}
mod siblings;
pub use siblings::*;

#[cfg(test)]
mod shared_wheel_tests;

/// The four real wheels of NVIDIA's `nvidia-cutlass-dsl-libs-*` 4.7.1 (convention
/// 83): one namespace tree `nvidia_cutlass_dsl/` filled by `-base` (compiled MLIR
/// half, `Root-Is-Purelib: false`), `-core` (pure Python, `true`) and the
/// `-cu12`/`-cu13` flavors (a `.so` each, `false`). The file lists are the
/// wheels' own, copied from the zips.
#[cfg(test)]
mod cutlass_split_wheels_tests;

#[cfg(test)]
mod adopt_sources_tests;
#[cfg(test)]
mod declared_sources_tests;
mod license;
pub use license::*;

#[cfg(test)]
mod sdist_license_tests;

#[cfg(test)]
mod prebuilt_library_tests;

#[cfg(test)]
mod generator_tests;

#[cfg(test)]
mod skeleton_header_guard_tests;

#[cfg(test)]
mod expand_order_tests;

mod arch_tags;
mod checks;
mod pytest_use;
pub use pytest_use::*;
mod placeholders;
pub use placeholders::*;
mod upstream_tag;
pub use arch_tags::*;
pub use upstream_tag::*;
#[cfg(test)]
mod requirement_tests;
#[cfg(test)]
mod review_fix_tests;
pub use checks::*;

#[cfg(test)]
mod sdist_files_tests;

#[cfg(test)]
mod license_guard_tests;

#[cfg(test)]
mod todo_placeholder_tests;

#[cfg(test)]
mod set_version_tests;

#[cfg(test)]
mod normalizer_tests;

#[cfg(test)]
mod license_collapse_tests;

#[cfg(test)]
mod missing_check_path_tests;

#[cfg(test)]
mod add_fixture_source_tests;

#[cfg(test)]
mod auto_fixture_source_tests;

#[cfg(test)]
mod sharp_edge_tests;

#[cfg(test)]
mod check_quality_tests;
#[cfg(test)]
mod pytest_use_tests;

#[cfg(test)]
mod console_script_tests;

#[cfg(test)]
mod dollar_macro_block_tests;

mod nested_python_expand;
pub use nested_python_expand::*;
#[cfg(test)]
mod nested_python_expand_tests;

mod python_expand_shape;
pub use python_expand_shape::*;
#[cfg(test)]
mod python_expand_shape_tests;

mod fixup_comment;
pub use fixup_comment::*;
#[cfg(test)]
mod fixup_comment_tests;

#[cfg(test)]
mod check_probe_tests;
mod deps;
pub use deps::*;

#[cfg(test)]
mod relax_bound_tests;

#[cfg(test)]
mod runtime_dependency_tests;

#[cfg(test)]
mod add_build_requires_tests;

#[cfg(test)]
mod conditional_dependency_tests;

#[cfg(test)]
mod hashed_libs_exclude_tests;

#[cfg(test)]
mod redist_requires_tests;

#[cfg(test)]
mod wheel_generator_tests;

#[cfg(test)]
mod license_url_injection_tests;

#[cfg(test)]
mod exclusive_arch_tests;
