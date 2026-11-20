//! Restricted product compositions built from the shared node runtime.

#[cfg(feature = "live")]
#[path = "../../../apps/live/src/runtime.rs"]
pub mod live;
pub(crate) mod platform_recovery;
pub(crate) mod recovery;
pub(crate) mod runtime;
#[cfg(feature = "sub2api-runtime")]
#[path = "../../../apps/sub2api/src/runtime.rs"]
pub mod sub2api;
