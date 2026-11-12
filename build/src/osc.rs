//! OBS/osc integration: package creation, checkout, source sync, and build.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

use ap_core::event::Event;

use crate::obs_status;
use crate::proc::{self, Captured, Emit};

mod branch;
pub use branch::*;
mod names;
pub use names::*;
mod error;
pub use error::*;

mod bounded_missing;
#[cfg(test)]
mod bounded_missing_tests;
mod failure;
#[cfg(test)]
mod query_retry_tests;
#[cfg(test)]
mod transient_text_tests;
pub use bounded_missing::*;
pub use failure::*;

#[cfg(test)]
mod extract_unresolvable_tests;

#[cfg(test)]
mod fuzzy_key_tests;

mod lookup;
pub use lookup::*;
#[cfg(test)]
mod lookup_tests;

mod distro;
#[cfg(test)]
mod strip_devel_wrapping_tests;
pub use distro::*;

#[cfg(test)]
mod distro_provides_tests;
mod repo_paths;
pub use repo_paths::*;
mod build_paths;
pub use build_paths::*;
#[cfg(test)]
mod build_paths_tests;
mod published;
pub use published::*;

#[cfg(test)]
mod tracked_stray_tests;

#[cfg(test)]
mod pending_changes_tests;

mod factory_name;
pub use factory_name::*;
mod project_packages;
#[cfg(test)]
mod published_binary_tests;
pub use project_packages::*;

mod files;
#[cfg(test)]
mod project_packages_cache_tests;
pub use files::*;

mod changes;
#[cfg(test)]
mod spec_file_name_tests;
pub use changes::*;
mod changes_dates;
mod changes_flag_lines;
pub use changes_dates::*;
pub use changes_flag_lines::*;
mod changes_normalise;
pub use changes_normalise::*;
mod changes_drop;
pub use changes_drop::*;
mod changes_merge;
pub use changes_merge::*;
mod changes_refresh;
pub use changes_refresh::*;
#[cfg(test)]
mod changes_file_tests;
#[cfg(test)]
mod changes_flag_lines_tests;
#[cfg(test)]
mod changes_merge_tests;
#[cfg(test)]
mod changes_normalise_tests;
mod update_notes;
pub use update_notes::*;
mod checkout;
pub use checkout::*;

#[cfg(test)]
mod failing_result_tests;

#[cfg(test)]
mod respelling_tests;

#[cfg(test)]
mod osc_error_text_tests;

#[cfg(test)]
mod unfinished_checkout_tests;

#[cfg(test)]
mod sandbox_spec_dir_tests;

#[cfg(test)]
mod xml_attr_and_status_tests;

#[cfg(test)]
mod changes_conflict_tests;

#[cfg(test)]
mod not_found_tests;
mod service;
mod service_root;
#[cfg(test)]
mod service_root_tests;
pub use service::*;
pub(crate) use service_root::{has_services_root, warn_if_no_root};
mod sandbox;
pub use sandbox::*;
mod ci_retry;
#[cfg(test)]
mod ci_retry_tests;
mod publish;
pub use publish::*;
mod rpmlint_sources;
pub use rpmlint_sources::*;
mod recipe_files;
use recipe_files::recipe_file_unused;
mod baseline;
pub use baseline::*;
mod baseline_commits;
pub use baseline_commits::*;
mod baseline_head;
pub use baseline_head::*;
mod baseline_adopt;
pub use baseline_adopt::*;
mod baseline_service_record;
pub use baseline_service_record::*;
#[cfg(test)]
mod baseline_adopt_tests;
#[cfg(test)]
mod baseline_commits_tests;
#[cfg(test)]
mod baseline_head_tests;
#[cfg(test)]
mod baseline_recover_tests;
#[cfg(test)]
mod baseline_service_record_tests;
#[cfg(test)]
mod recipe_files_tests;

#[cfg(test)]
mod baseline_tests;
mod status;
pub use status::*;
mod query;
pub use query::http_status;
use query::*;
mod build_retry;
pub use build_retry::*;
mod results;
use results::*;

#[cfg(test)]
mod results_cache_tests;

#[cfg(test)]
mod wait_pacing_tests;

mod binaries;
#[cfg(test)]
mod single_build_status_tests;
pub use binaries::*;
mod unresolvable;
pub use unresolvable::*;
mod repository;
pub use repository::*;
mod spec_info;
pub use spec_info::*;

mod poll;
#[cfg(test)]
mod sourceinfo_version_tests;
pub(crate) use poll::*;
mod build_once;
mod local_build;
mod wait_view;
#[cfg(test)]
mod wait_view_tests;
pub use local_build::*;
#[cfg(test)]
mod local_build_tests;

#[cfg(test)]
mod package_listing_tests;

#[cfg(test)]
mod obs_name_tests;

#[cfg(test)]
mod listing_failure_tests;

#[cfg(test)]
mod count_rpms_tests;

#[cfg(test)]
mod project_build_statuses_tests;

#[cfg(test)]
mod project_repo_paths_tests;

#[cfg(test)]
mod rpmspec_sandbox_tests;

#[cfg(test)]
mod publish_plan_tests;
#[cfg(test)]
mod rpmlint_sources_tests;

#[cfg(test)]
mod service_validation_tests;

#[cfg(test)]
mod dash_name_tests;

#[cfg(test)]
mod repo_arch_tests;

#[cfg(test)]
mod poll_tests;

#[cfg(test)]
mod overall_status_tests;

#[cfg(test)]
mod superseded_rpm_tests;

#[cfg(test)]
mod redist_name_tests;

#[cfg(test)]
mod transient_broken_tests;

#[cfg(test)]
mod transient_error_tests;

#[cfg(test)]
mod tool_missing_tests;
