//! Node speed-test protocol and lease coordination.

#[cfg(feature = "node-runtime")]
pub mod control;
pub mod memory;
#[cfg(feature = "node-runtime")]
pub mod runtime;
#[cfg(feature = "node-runtime")]
pub mod wire;
