//! Shared atomic local-file replacement.

#[cfg(feature = "standalone")]
pub(crate) use veytrane_transport::atomic::WriteOutcome;
pub(crate) use veytrane_transport::atomic::write_json;
#[cfg(feature = "node-runtime")]
pub(crate) use veytrane_transport::atomic::{is_reparse, regular_file};
