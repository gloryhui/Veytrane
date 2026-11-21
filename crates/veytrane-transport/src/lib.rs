//! Shared transport primitives for Veytrane.

pub mod atomic;
#[cfg(feature = "quic")]
pub mod channel_binding;
#[cfg(feature = "quic")]
pub mod connection;
pub mod error;
#[cfg(feature = "quic")]
pub mod handshake;
pub mod identity;
pub mod identity_wire;
pub mod instance_lock;
#[cfg(feature = "legacy-control")]
pub mod legacy_control;
pub mod operation_id;
#[cfg(feature = "private-storage")]
pub mod permissions;
#[cfg(feature = "quic")]
pub mod punch;
#[cfg(feature = "quic")]
pub mod quic;
pub mod raw_frame;
pub mod short_id;
#[cfg(feature = "signal-tls")]
pub mod signal_client;
#[cfg(feature = "signal-tls")]
pub mod signal_tls;
#[cfg(feature = "quic")]
pub mod signal_wire;
#[cfg(feature = "quic")]
pub mod udp;

/// Maximum silence while waiting for data on an active transfer stream.
pub const TRANSFER_IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// 等待一条连接的第一条双向业务流的最大时间。
pub const ACCEPT_FIRST_BI_STREAM_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// 等待业务流首帧的最大时间。
pub const STREAM_FIRST_FRAME_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
