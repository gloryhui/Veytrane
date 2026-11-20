//! Shared atomic local-file replacement.

pub(crate) use veytrane_transport::atomic::{WriteOutcome, write_json};
#[cfg(feature = "agent-base")]
pub(crate) use veytrane_transport::atomic::{is_reparse, regular_file};
