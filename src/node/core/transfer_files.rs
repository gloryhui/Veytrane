//! Compatibility re-exports for blocking desktop file operations.

pub(crate) use veytrane_files::transfer_files::{
    VerifiedSource, accept_directory, accept_entry, begin_attempt, bound_task,
    cleanup_skipped_staging, cleanup_staging, delete_completed_receive_file, failure, local_error,
    open_download_with_space, publish, scan_selected_file, source_chunk, source_parent, transition,
    verify_source,
};

#[cfg(test)]
pub(crate) use veytrane_files::transfer_files::{open_download, select_file, stage_dir};
