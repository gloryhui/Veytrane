//! CLI and HTTP entry points over the node APIs.

#[cfg(feature = "node")]
pub mod cli;
#[cfg(feature = "agent-web")]
pub mod navigation;
#[cfg(feature = "node")]
pub mod web;
