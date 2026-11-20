//! Root compatibility path for safe storage primitives.

pub use veytrane_files::secure_fs::{
    FileIdentity, atomic_json, child, fail, identity, name_key, open, parent, rename_no_replace,
    root, sync,
};
