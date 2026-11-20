//! File manifests, resumable downloads, and safe file storage for Veytrane.

#[cfg(feature = "node-tasks")]
pub mod auto_resume;
#[cfg(feature = "node-tasks")]
pub mod bandwidth;
pub mod chunk_bitmap;
pub mod chunker;
pub mod download;
#[cfg(feature = "node-tasks")]
pub mod frame_budget;
pub mod manifest;
#[cfg(feature = "node-tasks")]
pub mod protocol;
#[cfg(feature = "node-tasks")]
pub mod queue;
#[cfg(feature = "node-tasks")]
pub mod receive_policy;
pub mod relative_path;
#[cfg(feature = "safe-storage")]
pub mod secure_fs;
#[cfg(all(feature = "node-tasks", feature = "safe-storage"))]
pub mod space_budget;
#[cfg(all(feature = "node-tasks", feature = "safe-storage"))]
pub mod task_events;
#[cfg(feature = "node-tasks")]
pub mod task_measurements;
#[cfg(feature = "node-tasks")]
pub mod task_model;
#[cfg(all(feature = "node-tasks", feature = "safe-storage"))]
pub mod task_recovery;
#[cfg(all(feature = "node-tasks", feature = "safe-storage"))]
pub mod task_snapshot;
#[cfg(all(feature = "node-tasks", feature = "safe-storage"))]
pub mod task_store;

use veytrane_transport::error;
