//! Authenticated, bounded UDP fallback. The relay never terminates QUIC or parses business data.
pub mod client;
#[cfg(any(feature = "signaling-server", test))]
#[path = "../../apps/signaling/src/relay.rs"]
pub mod server;
mod wire;

pub const RELAY_FALLBACK_DELAY: std::time::Duration = std::time::Duration::from_millis(2500);
pub use veytrane_transport::quic::relay::{RELAY_ATTEMPT_TIMEOUT, RELAY_BIND_TIMEOUT};

#[cfg(test)]
pub(crate) mod tests;
