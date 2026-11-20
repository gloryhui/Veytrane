//! Shared current-user directory protection.

#[cfg(windows)]
pub(crate) use veytrane_transport::permissions::windows;
pub use veytrane_transport::permissions::{ensure_directory, ensure_private_app_dir, user_key};
