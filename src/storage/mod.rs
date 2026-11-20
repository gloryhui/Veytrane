//! Root compatibility paths for shared file storage plus local OS primitives.

pub(crate) mod atomic;
pub mod chunk_bitmap;
#[cfg(feature = "node")]
pub mod permissions;
#[cfg(feature = "node")]
pub mod relative_path;
#[cfg(feature = "node")]
pub(crate) mod secure_fs;

pub use veytrane_files::download::PartialDownload;
