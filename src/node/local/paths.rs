use std::{
    io,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug)]
pub struct AgentPaths {
    pub root: PathBuf,
    pub endpoint: String,
}
impl AgentPaths {
    pub fn default_root() -> io::Result<PathBuf> {
        Ok(dirs::data_local_dir()
            .ok_or_else(|| io::Error::other("没有用户数据目录"))?
            .join("p2p-file")
            .join("node"))
    }

    pub fn new(root: &Path) -> io::Result<Self> {
        crate::storage::permissions::ensure_directory(root)?;
        let root = root.canonicalize()?;
        #[cfg(windows)]
        let endpoint = {
            let key = format!(
                "{}:{}",
                crate::storage::permissions::user_key()?,
                root.to_string_lossy().to_lowercase()
            );
            format!(
                r"\\.\pipe\p2p-file-{}",
                &blake3::hash(key.as_bytes()).to_hex()[..32]
            )
        };
        #[cfg(unix)]
        let endpoint = root
            .join("agent.sock")
            .to_str()
            .ok_or_else(|| io::Error::other("IPC路径不是UTF-8"))?
            .to_owned();
        Ok(Self { root, endpoint })
    }
    pub fn config(&self) -> PathBuf {
        self.root.join("node.json")
    }
    pub fn identity(&self) -> PathBuf {
        self.root.join("identity.key")
    }
    pub fn tasks(&self) -> PathBuf {
        self.root.join("tasks.json")
    }
    pub fn lock(&self) -> PathBuf {
        self.root.join("agent.lock")
    }
    pub fn identity_lock(
        id: crate::identity::NodeId,
    ) -> io::Result<crate::instance_lock::InstanceLock> {
        crate::identity::acquire_identity_lease(id).map_err(io::Error::other)
    }
}
