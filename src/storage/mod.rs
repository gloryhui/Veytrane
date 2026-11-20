//! Root compatibility paths for shared file storage plus local OS primitives.

#[cfg(any(feature = "standalone", feature = "node-runtime"))]
pub(crate) mod atomic;
#[cfg(feature = "standalone")]
pub mod chunk_bitmap;
#[cfg(feature = "node-runtime")]
pub mod permissions;
#[cfg(feature = "standalone")]
pub mod relative_path;

#[cfg(feature = "standalone")]
pub use veytrane_files::download::PartialDownload;
