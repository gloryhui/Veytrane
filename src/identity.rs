//! Root identity path plus the node-only OS lease wrapper.

pub use veytrane_transport::identity::{
    Identity, NODE_ID_LEN, NodeId, PUBLIC_KEY_LEN, SECRET_KEY_LEN, public_key_from_bytes,
    signature_from_bytes, verify_signature,
};

use crate::error::{Error, Result};

/// All local networking entry points hold this lease for their full lifetime.
/// It is keyed by public identity, independent of the key/data directory.
pub fn acquire_identity_lease(id: NodeId) -> Result<crate::instance_lock::InstanceLock> {
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(std::path::PathBuf::from)
        .ok_or_else(|| Error::Identity("没有当前用户主目录，无法取得身份锁".into()))?;
    let root = home.join(".p2p-file").join("identity-locks");
    #[cfg(feature = "node")]
    crate::storage::permissions::ensure_directory(&root)?;
    crate::instance_lock::InstanceLock::acquire(&root.join(format!("{id}.lock")))
        .map_err(|e| Error::Identity(e.to_string()))
}
