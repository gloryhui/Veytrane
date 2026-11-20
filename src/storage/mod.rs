//! Root compatibility paths for shared file storage plus local OS primitives.

pub(crate) mod atomic;
pub mod chunk_bitmap;
#[cfg(feature = "node")]
pub mod permissions;
#[cfg(feature = "node")]
pub mod relative_path;

pub use veytrane_files::download::PartialDownload;
