//! Build orchestration: log extraction, spec generation, the agent turn loop
//! and the validation gate.

pub mod agent;
mod conventions_table;
pub mod depclass;
pub mod depreach;
pub mod deselect;
pub mod discovery;
pub mod distro_bounds;
pub mod doctor;
pub mod explain;
pub mod flaky;
pub mod flavors;
pub mod http;
pub mod log;
pub mod nvidia_spec;
pub mod obs_status;
pub mod osc;
pub mod package_notes;
pub mod pipeline;
pub mod proc;
pub mod quality;
pub mod redist;
pub mod ship_condition;
pub mod sources;
pub mod spec;
pub mod spec_tidy;
pub mod stage;
pub mod step;
pub mod update;
pub mod update_project;
pub mod upstream;
pub mod upstream_cache;
pub mod version;
pub mod workspace;
