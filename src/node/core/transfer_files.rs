//! Compatibility re-exports for blocking desktop file operations.

pub(crate) use veytrane_files::transfer_files::failure;

#[cfg(test)]
pub(crate) use veytrane_files::transfer_files::open_download;
