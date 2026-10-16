//! Build orchestration: log extraction, spec generation, the agent turn loop
//! and the validation gate.

pub mod agent;
pub mod depclass;
pub mod depreach;
pub mod deselect;
pub mod discovery;
pub mod doctor;
pub mod distro_bounds;
pub mod flavors;
pub mod http;
pub mod log;
pub mod obs_status;
pub mod nvidia_spec;
pub mod osc;
pub mod package_notes;
pub mod pipeline;
pub mod proc;
pub mod redist;
pub mod sources;
pub mod spec;
pub mod stage;
pub mod step;
pub mod update;
pub mod update_project;
pub mod workspace;
