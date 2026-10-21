//! Unified node with a headless core, process host, local APIs and presentation adapters.
//! New code should import the explicit layer paths below.

pub mod adapters;
pub mod api;
pub mod core;
pub mod host;
pub mod local;
pub mod products;

// Stable public paths retained for existing library consumers.
pub use crate::instance_lock;
pub use crate::node::adapters::cli as management_cli;
pub use crate::node::adapters::migration;
pub use crate::node::adapters::web as agent_web;
pub use crate::node::api::build_info;
pub use crate::node::api::client as agent_client;
pub use crate::node::api::protocol as ipc;
pub use crate::node::core::authorization;
pub use crate::node::core::bandwidth;
pub use crate::node::core::config;
pub use crate::node::core::control;
pub use crate::node::core::diagnostics;
pub use crate::node::core::dispatch;
pub use crate::node::core::preferences;
pub use crate::node::core::protocol;
pub use crate::node::core::receive_policy;
pub use crate::node::core::remote_auth;
pub use crate::node::core::services;
pub use crate::node::core::settings as agent_config;
pub use crate::node::core::snapshot;
pub use crate::node::core::task_details;
pub use crate::node::core::task_events;
pub use crate::node::core::task_model;
pub use crate::node::core::task_recovery;
pub use crate::node::core::task_store;
pub use crate::node::core::trusted_devices;
pub use crate::node::core::wire;
pub use crate::node::host::agent;
pub use crate::node::local::paths as agent_paths;
pub use crate::node::local::transport as ipc_transport;
pub use crate::node::products::sub2api as restricted_sub2api;
pub use crate::storage::permissions as private_access;
