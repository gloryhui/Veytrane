//! File manifests, resumable downloads, and safe file storage for Veytrane.

pub mod chunk_bitmap;
pub mod download;
pub mod manifest;
pub mod relative_path;
#[cfg(feature = "safe-storage")]
pub mod secure_fs;
#[cfg(feature = "node-tasks")]
pub mod task_model;

use veytrane_transport::error;
