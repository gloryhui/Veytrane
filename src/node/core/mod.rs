//! Headless node business runtime, configuration and durable task state. No host or UI dependencies.

pub(crate) mod activity;
pub mod authorization;
pub(crate) mod auto_resume;
pub mod bandwidth;
pub mod config;
pub mod connectivity;
pub mod control;
pub mod diagnostics;
pub mod dispatch;
pub(crate) mod drop_send;
pub(crate) mod files;
pub(crate) mod frame_budget;
pub(crate) mod incremental;
pub(crate) mod network_state;
pub mod protocol;
pub(crate) mod publish;
pub(crate) mod queue;
pub mod receive_policy;
pub mod remote_auth;
pub mod services;
pub(crate) mod session;
pub mod settings;
pub mod snapshot;
pub(crate) mod space_budget;
pub(crate) mod speed;
pub mod task_details;
pub mod task_events;
pub mod task_model;
pub mod task_recovery;
pub mod task_store;
pub(crate) mod transfer;
pub(crate) mod transfer_files;
pub mod trusted_devices;
pub mod wire;

#[cfg(test)]
mod process_tests;
