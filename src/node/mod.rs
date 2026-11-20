//! Unified node with a headless core, process host, local APIs and presentation adapters.
//! Import the explicit layer paths below.

#[cfg(feature = "node-cli")]
pub mod adapters;
#[cfg(feature = "agent-base")]
pub mod api;
pub mod composition;
pub mod core;
#[cfg(feature = "agent-base")]
#[path = "../../apps/agent/src/host/mod.rs"]
pub mod host;
#[cfg(feature = "agent-base")]
pub mod local;
#[cfg(any(feature = "live", feature = "sub2api-runtime"))]
pub mod products;
