//! Headless node business runtime, configuration and durable task state. No host or UI dependencies.

pub(crate) mod activity;
pub mod authorization;
pub mod bandwidth;
pub mod config;
pub mod connectivity;
pub mod control;
pub mod diagnostics;
pub mod dispatch;
#[cfg(feature = "agent-files")]
pub(crate) mod drop_send;
pub(crate) mod frame_budget;
#[cfg(feature = "agent-files")]
pub(crate) mod incremental;
pub(crate) mod network_state;
pub mod protocol;
#[cfg(feature = "agent-files")]
pub(crate) mod queue;
pub mod receive_policy;
pub mod remote_auth;
pub mod services;
pub(crate) mod session;
pub mod snapshot;
#[cfg(feature = "agent-files")]
pub(crate) mod space_budget;
pub(crate) mod speed;
#[cfg(all(test, feature = "agent-files"))]
pub(crate) mod speed_wire;
#[cfg(feature = "agent-files")]
pub mod task_details;
#[cfg(feature = "agent-files")]
pub mod task_events;
#[cfg(feature = "agent-files")]
pub mod task_model;
#[cfg(feature = "agent-files")]
pub mod task_recovery;
#[cfg(feature = "agent-files")]
pub mod task_store;
#[cfg(feature = "agent-files")]
pub(crate) mod transfer;
#[cfg(feature = "agent-files")]
pub(crate) mod transfer_files;
pub mod trusted_devices;
pub mod wire;

#[cfg(all(test, feature = "node"))]
mod process_tests;
