//! Compatibility re-exports for external-path admission.

pub use veytrane_files::drop_send::Admission;
pub(crate) use veytrane_files::drop_send::{Selection, classify, normalize, validate_paths};
