//! Shared workspace, run evidence, and engineering memory for AEP.
pub mod autopilot;
pub mod cancel;
pub mod delivery;
pub mod deploy_keys;
pub mod launch;
pub mod memory;
pub mod memory_tidy;
pub mod studio;
pub mod task;
pub mod transcript;
pub mod usage;
pub mod work_draft;
pub mod workspace;

pub const CONTROL_OPERATOR_TOKEN: &str = "AEP_CONTROL_OPERATOR_TOKEN";
pub const CONTROL_DEVICE_TOKEN: &str = "AEP_CONTROL_DEVICE_TOKEN";
pub mod workflow;

pub mod handoff;
pub mod project;
pub mod session_reuse;
pub mod zcode_catalog;
pub mod zcode_receipt;
