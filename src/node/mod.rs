//! Shared node services. No dependency on a GUI or command-line presentation.
pub mod bandwidth;
pub mod config;
pub mod preferences;
#[allow(dead_code)] // Preserve the existing staged model/recovery API allowance.
pub mod protocol;
pub mod receive_policy;
pub mod remote_auth;
#[allow(dead_code)] // Preserve the existing staged model/recovery API allowance.
pub mod task_events;
#[allow(dead_code)] // Preserve the existing staged model/recovery API allowance.
pub mod task_model;
#[allow(dead_code)] // Preserve the existing staged model/recovery API allowance.
pub mod task_recovery;
#[allow(dead_code)] // Preserve the existing staged model/recovery API allowance.
pub mod task_store;
pub mod trusted_devices;

pub(crate) mod activity;
pub(crate) mod auto_resume;
pub(crate) mod drop_send;
pub(crate) mod files;
pub(crate) mod frame_budget;
pub(crate) mod incremental;
pub mod instance_lock;
pub(crate) mod network_state;
pub(crate) mod publish;
pub(crate) mod queue;
pub(crate) mod secure_fs;
pub mod snapshot;
pub(crate) mod space_budget;
pub(crate) mod speed;
pub mod task_details;
pub(crate) mod transfer;
pub(crate) mod transfer_files;
pub(crate) mod tunnel;
pub(crate) use snapshot as ui_model;
pub mod diagnostics;
pub(crate) mod session;

pub mod authorization;
pub mod control;
pub mod dispatch;
pub mod wire;
