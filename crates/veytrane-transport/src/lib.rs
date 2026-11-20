//! Shared transport primitives for Veytrane.

pub mod atomic;
#[cfg(feature = "quic")]
pub mod channel_binding;
pub mod error;
#[cfg(feature = "quic")]
pub mod handshake;
pub mod identity;
pub mod identity_wire;
pub mod instance_lock;
#[cfg(feature = "private-storage")]
pub mod permissions;
pub mod raw_frame;

/// Maximum silence while waiting for data on an active transfer stream.
pub const TRANSFER_IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
