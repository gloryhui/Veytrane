//! Explicit stopped-owner import. No source file or business file is modified.
use super::{
    agent_config::{ConfigRecord, NodeSettings},
    agent_paths::AgentPaths,
    authorization::{PeerResources, TrustSource},
    config::{DesktopConfig, SettingsDraft},
    wire::PeerWireMode,
};
use crate::{
    identity::{Identity, NodeId},
    instance_lock::InstanceLock,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

const MARKER: &str = "migration.pending.json";
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Pending {
    schema_version: u32,
    preview: String,
    backup: String,
    source_data: PathBuf,
    node_id: String,
    files: Vec<(String, String)>,
}
struct Source {
    _locks: Vec<InstanceLock>,
    identity: Identity,
    config: zeroize::Zeroizing<Vec<u8>>,
    tasks: Vec<u8>,
    settings: NodeSettings,
    summary: Value,
    fingerprint: String,
}
fn read_regular(path: &Path, limit: u64) -> io::Result<Vec<u8>> {
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > limit {
        return Err(io::Error::other("迁移源必须是有界普通文件"));
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err(io::Error::other("迁移源不能是reparse point"));
        }
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(limit + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(io::Error::other("迁移源超过上限"));
    }
    Ok(bytes)
}
fn source_locks(data: &Path) -> io::Result<Vec<InstanceLock>> {
    if !data.is_absolute() || !data.is_dir() {
        return Err(io::Error::other("源数据目录必须存在且为绝对路径"));
    }
    Ok(vec![
        InstanceLock::acquire(&data.join("instance.lock")).map_err(io::Error::other)?,
        InstanceLock::acquire(&data.join("tasks.lock")).map_err(io::Error::other)?,
    ])
}
fn load_source(config: &Path, data: &Path) -> io::Result<Source> {
    let mut locks = source_locks(data)?;
    let identity = Identity::load(&data.join("identity.key")).map_err(io::Error::other)?;
    locks.push(AgentPaths::identity_lock(identity.node_id())?);
    let bytes = zeroize::Zeroizing::new(read_regular(config, 1024 * 1024)?);
    let loaded = DesktopConfig::decode(&bytes).map_err(io::Error::other)?;
    let draft = SettingsDraft::from_config(loaded);
    let tasks_path = data.join("tasks.json");
    let tasks = if tasks_path.try_exists()? {
        read_regular(&tasks_path, 16 * 1024 * 1024)?
    } else {
        serde_json::to_vec(&json!({"format_version":1,"generation":0,"tasks":[]}))
            .map_err(io::Error::other)?
    };
    let records = super::task_store::inspect_bytes(&tasks).map_err(io::Error::other)?;
    let root = draft
        .receive_directory
        .clone()
        .ok_or_else(|| io::Error::other("旧桌面未选择接收目录，请先在旧端明确设置后再导入"))?;
    let mut settings = NodeSettings::initial(root);
    settings.signal_server = if draft.signal_host.is_empty() {
        String::new()
    } else if draft.signal_host.contains(':') {
        format!("[{}]:{}", draft.signal_host, draft.signal_port)
    } else {
        format!("{}:{}", draft.signal_host, draft.signal_port)
    };
    settings.signal_tls = draft.signal_tls;
    settings.relay_server = draft.relay_server;
    settings.remote_auth = draft.remote_auth;
    settings.trusted_devices = draft.trusted_devices;
    settings.services = draft.allowed_forward_targets;
    settings.forwards = draft.tunnel_rules;
    settings.auto_resume = draft.auto_resume;
    settings.send_concurrency = draft.send_concurrency;
    settings.file_limits = draft.file_limits;
    settings.receive_safety_mib = draft.receive_safety_mib;
    let mut peers = BTreeSet::new();
    for device in &settings.trusted_devices {
        settings
            .trust_sources
            .insert(device.node_id.clone(), TrustSource::LegacyImport);
        peers.insert(device.node_id.clone());
    }
    for service in &settings.services {
        for peer in &service.allowed_peers {
            peers.insert(peer.clone());
        }
    }
    for rule in &settings.forwards {
        peers.insert(rule.peer_node_id.clone());
    }
    for task in &records {
        peers.insert(task.peer_id().as_str().to_owned());
    }
    for peer in &peers {
        NodeId::from_hex(peer).map_err(io::Error::other)?;
        settings
            .peer_modes
            .insert(peer.clone(), PeerWireMode::LegacyDesktopV1);
        settings.resources.peers.push(PeerResources {
            node_id: peer.clone(),
            files: true,
            speed: true,
            disabled: false,
        });
    }
    settings.validate().map_err(io::Error::other)?;
    let mut hash = blake3::Hasher::new();
    hash.update(identity.node_id().as_bytes());
    hash.update(
        config
            .canonicalize()?
            .as_os_str()
            .to_string_lossy()
            .as_bytes(),
    );
    hash.update(
        data.canonicalize()?
            .as_os_str()
            .to_string_lossy()
            .as_bytes(),
    );
    hash.update(&bytes);
    hash.update(&tasks);
    let fingerprint = hash.finalize().to_hex().to_string();
    let summary = json!({"preview":fingerprint,"node_id":identity.node_id().to_hex(),"tasks":records.len(),"known_peers":peers.len(),"trusted_devices":settings.trusted_devices.len(),"file_conflict_policy":"desktop_backup_publish","compatibility":"known_peers_legacy_desktop_v1","limitations":["未知旧设备必须显式设置协议模式及资源授权","保留原接收根和任务绝对绑定；不移动业务文件","旧CLI不覆盖策略无等价适配，拒绝作为桌面导入"]});
    Ok(Source {
        _locks: locks,
        identity,
        config: bytes,
        tasks,
        settings,
        summary,
        fingerprint,
    })
}
fn ensure_empty(paths: &AgentPaths) -> io::Result<()> {
    for name in ["node.json", "identity.key", "tasks.json", MARKER] {
        if paths.root.join(name).try_exists()? {
            return Err(io::Error::other(format!(
                "目标已有{name}，拒绝覆盖；中断导入请使用migrate-resume"
            )));
        }
    }
    Ok(())
}
pub fn preview(paths: &AgentPaths, config: &Path, data: &Path) -> io::Result<Value> {
    let _target = InstanceLock::acquire(&paths.lock()).map_err(io::Error::other)?;
    ensure_empty(paths)?;
    Ok(load_source(config, data)?.summary)
}
pub fn import(paths: &AgentPaths, config: &Path, data: &Path, expected: &str) -> io::Result<Value> {
    let _target = InstanceLock::acquire(&paths.lock()).map_err(io::Error::other)?;
    ensure_empty(paths)?;
    let source = load_source(config, data)?;
    if source.fingerprint != expected {
        return Err(io::Error::other("preview_conflict: 源已变化，请重新预览"));
    }
    let name = format!(".migration-backup-{:032x}", rand::random::<u128>());
    let backup = paths.root.join(&name);
    super::private_access::ensure_directory(&backup)?;
    source
        .identity
        .save(&backup.join("identity.key"))
        .map_err(io::Error::other)?;
    write_new(&backup.join("legacy-settings.json"), &source.config)?;
    write_new(&backup.join("tasks.json"), &source.tasks)?;
    let record = ConfigRecord {
        schema_version: 1,
        revision: 0,
        last_request: None,
        settings: source.settings.clone(),
    };
    let mut root = serde_json::to_vec_pretty(&record).map_err(io::Error::other)?;
    root.push(b'\n');
    write_new(&backup.join("node.json"), &root)?;
    let files = ["identity.key", "tasks.json", "node.json"]
        .iter()
        .map(|name| {
            read_regular(&backup.join(name), 16 * 1024 * 1024).map(|bytes| {
                (
                    (*name).to_owned(),
                    blake3::hash(&bytes).to_hex().to_string(),
                )
            })
        })
        .collect::<io::Result<Vec<_>>>()?;
    for name in [
        "identity.key",
        "legacy-settings.json",
        "tasks.json",
        "node.json",
    ] {
        let path = backup.join(name);
        let mut permissions = std::fs::metadata(&path)?.permissions();
        permissions.set_readonly(true);
        std::fs::set_permissions(path, permissions)?;
    }
    #[cfg(unix)]
    std::fs::File::open(&backup)?.sync_all()?;
    let pending = Pending {
        schema_version: 1,
        preview: source.fingerprint,
        backup: name,
        source_data: data.canonicalize()?,
        node_id: source.identity.node_id().to_hex(),
        files,
    };
    crate::storage::atomic::write_json(&paths.root.join(MARKER), &pending)?;
    finish(paths, &pending, &source.identity)?;
    Ok(json!({"imported":true,"node_id":pending.node_id,"backup":backup,"source_unchanged":true}))
}
pub fn resume(paths: &AgentPaths, expected: &str) -> io::Result<Value> {
    let _target = InstanceLock::acquire(&paths.lock()).map_err(io::Error::other)?;
    let pending: Pending =
        serde_json::from_slice(&read_regular(&paths.root.join(MARKER), 1024 * 1024)?)
            .map_err(io::Error::other)?;
    validate_pending(&pending)?;
    if pending.preview != expected {
        return Err(io::Error::other("preview_conflict: 预览指纹不匹配"));
    }
    let _source = source_locks(&pending.source_data)?;
    let identity = Identity::load(&paths.root.join(&pending.backup).join("identity.key"))
        .map_err(io::Error::other)?;
    let _identity = AgentPaths::identity_lock(identity.node_id())?;
    if identity.node_id().to_hex() != pending.node_id {
        return Err(io::Error::other("备份身份不匹配"));
    }
    finish(paths, &pending, &identity)?;
    Ok(json!({"imported":true,"node_id":pending.node_id,"resumed":true}))
}
fn validate_pending(pending: &Pending) -> io::Result<()> {
    if pending.schema_version != 1
        || !pending.backup.starts_with(".migration-backup-")
        || pending.backup.contains(['/', '\\'])
        || pending.files.len() != 3
        || !pending.source_data.is_absolute()
    {
        return Err(io::Error::other("导入标记无效"));
    }
    let names = pending
        .files
        .iter()
        .map(|(name, _)| name.as_str())
        .collect::<BTreeSet<_>>();
    if names != BTreeSet::from(["identity.key", "tasks.json", "node.json"]) {
        return Err(io::Error::other("导入标记文件集合无效"));
    }
    Ok(())
}
fn finish(paths: &AgentPaths, pending: &Pending, identity: &Identity) -> io::Result<()> {
    validate_pending(pending)?;
    let backup = paths.root.join(&pending.backup);
    // Validate every backup and every existing destination before any write.
    let mut contents = Vec::new();
    for (name, hash) in &pending.files {
        let bytes = zeroize::Zeroizing::new(read_regular(&backup.join(name), 16 * 1024 * 1024)?);
        if blake3::hash(&bytes).to_hex().as_str() != hash {
            return Err(io::Error::other("备份摘要不匹配"));
        }
        let destination = paths.root.join(name);
        if destination.try_exists()?
            && read_regular(&destination, 16 * 1024 * 1024)?.as_slice() != bytes.as_slice()
        {
            return Err(io::Error::other("目标已变化，拒绝覆盖新文件/授权/任务"));
        }
        contents.push((name.as_str(), bytes));
    }
    let config = ConfigRecord::load(&backup.join("node.json")).map_err(io::Error::other)?;
    if config.revision != 0 || identity.node_id().to_hex() != pending.node_id {
        return Err(io::Error::other("备份根或身份无效"));
    }
    super::task_store::inspect_bytes(&read_regular(&backup.join("tasks.json"), 16 * 1024 * 1024)?)
        .map_err(io::Error::other)?;
    // Root is the last file published. Agent refuses a pending import.
    for name in ["identity.key", "tasks.json", "node.json"] {
        let destination = paths.root.join(name);
        if destination.try_exists()? {
            continue;
        }
        if name == "identity.key" {
            identity.save(&destination).map_err(io::Error::other)?;
        } else {
            let bytes = &contents.iter().find(|(n, _)| *n == name).unwrap().1;
            let outcome = crate::storage::atomic::write_bytes(&destination, bytes)?;
            if let Some(warning) = outcome.durability_warning {
                tracing::warn!(%warning);
            }
        }
    }
    std::fs::remove_file(paths.root.join(MARKER))?;
    #[cfg(unix)]
    std::fs::File::open(&paths.root)?.sync_all()?;
    Ok(())
}
fn write_new(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}
pub fn guard_initialization(paths: &AgentPaths) -> io::Result<()> {
    if paths.root.join(MARKER).try_exists()? {
        return Err(io::Error::other(
            "存在中断导入，请migrate-resume，禁止生成身份",
        ));
    }
    let default = AgentPaths::default_root()?;
    if default.try_exists()?
        && default.canonicalize()? == paths.root
        && super::config::AppPaths::discover()
            .map_err(io::Error::other)?
            .identity_file()
            .try_exists()?
    {
        return Err(io::Error::other(
            "检测到旧桌面身份，请先migrate-preview/migrate-desktop；拒绝另建身份",
        ));
    }
    Ok(())
}
