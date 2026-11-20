//! Shared transport primitives for Veytrane.

pub mod atomic;
pub mod error;
pub mod identity;
pub mod instance_lock;
#[cfg(feature = "private-storage")]
pub mod permissions;
pub mod raw_frame;
