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
#[cfg(feature = "quic")]
pub mod quic;
pub mod raw_frame;
#[cfg(feature = "quic")]
pub mod udp;

/// Maximum silence while waiting for data on an active transfer stream.
pub const TRANSFER_IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
