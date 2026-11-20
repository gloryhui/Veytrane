//! Unified node with a headless core, process host, local APIs and presentation adapters.
//! Import the explicit layer paths below.

#[cfg(feature = "node")]
pub mod adapters;
pub mod api;
pub mod composition;
pub mod core;
pub mod host;
pub mod local;
#[cfg(feature = "node")]
pub mod products;
