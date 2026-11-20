//! One revision coordinator for every management entry point.
use crate::node::api::protocol as ipc;
use crate::node::composition::forwarding;
use crate::node::composition::settings::{ConfigRecord, NodeSettings};
use crate::node::host::agent::Agent;
use crate::node::local::paths::AgentPaths;
use serde_json::{Value, json};
use std::{
    io::{self, Read},
    path::Path,
};

use crate::node::core::session::CommitOptions;

pub fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> io::Result<T> {
    let mut bytes = zeroize::Zeroizing::new(Vec::new());
    let input: Box<dyn Read> = if path == Path::new("-") {
        Box::new(io::stdin())
    } else {
        Box::new(std::fs::File::open(path)?)
    };
    input
        .take(ipc::FRAME_LIMIT as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > ipc::FRAME_LIMIT {
        return Err(io::Error::other("输入超过1MiB"));
    }
    serde_json::from_slice(&bytes).map_err(io::Error::other)
}

pub fn ownership_status(paths: &AgentPaths) -> io::Result<Value> {
    use crate::identity::{Identity, acquire_identity_lease};
    use crate::instance_lock::InstanceLock;

    let _directory = InstanceLock::acquire(&paths.lock()).map_err(io::Error::other)?;
    let identity = if paths.identity().try_exists()? {
        Some(Identity::load(&paths.identity()).map_err(io::Error::other)?)
    } else {
        None
    };
    let _identity = identity
        .as_ref()
        .map(|identity| acquire_identity_lease(identity.node_id()))
        .transpose()
        .map_err(io::Error::other)?;
    let tasks_lock = paths.tasks().with_extension("lock");
    let _tasks = match std::fs::symlink_metadata(&tasks_lock) {
        Ok(_) => Some(InstanceLock::acquire(&tasks_lock).map_err(io::Error::other)?),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    Ok(json!({"running":false,"node_id":identity.map(|id|id.node_id().to_hex())}))
}

pub fn initialize(paths: &AgentPaths) -> io::Result<Value> {
    let _lock =
        crate::instance_lock::InstanceLock::acquire(&paths.lock()).map_err(io::Error::other)?;
    if paths.config().try_exists()? {
        return Err(io::Error::other("节点配置已存在"));
    }
    let identity =
        crate::identity::Identity::load_or_create(&paths.identity()).map_err(io::Error::other)?;
    let _identity = crate::identity::acquire_identity_lease(identity.node_id())
        .map_err(io::Error::other)?;
    let receive = paths.root.join("receive");
    ConfigRecord::initialize(&paths.config(), receive).map_err(io::Error::other)?;
    Ok(json!({"node_id":identity.node_id().to_hex(),"config":paths.config()}))
}
pub fn offline_apply(paths: &AgentPaths, file: &Path, expected: u64) -> io::Result<Value> {
    let _lock =
        crate::instance_lock::InstanceLock::acquire(&paths.lock()).map_err(io::Error::other)?;
    let identity = crate::identity::Identity::load(&paths.identity()).map_err(io::Error::other)?;
    let _identity = crate::identity::acquire_identity_lease(identity.node_id())
        .map_err(io::Error::other)?;
    let mut record = ConfigRecord::load(&paths.config()).map_err(io::Error::other)?;
    if record.revision != expected {
        return Err(io::Error::other(format!(
            "revision_conflict: 当前revision为{}",
            record.revision
        )));
    }
    let mut value: Value = read_json(file)?;
    let object = value
        .as_object_mut()
        .ok_or_else(|| io::Error::other("配置草稿必须是对象"))?;
    if object.contains_key("remote_auth") {
        return Err(io::Error::other("草稿不得包含验证器"));
    }
    object.insert(
        "remote_auth".into(),
        serde_json::to_value(&record.settings.remote_auth).map_err(io::Error::other)?,
    );
    let mut settings: NodeSettings = serde_json::from_value(value).map_err(io::Error::other)?;
    for device in &settings.trusted_devices {
        let source = settings
            .trust_sources
            .get(&device.node_id)
            .ok_or_else(|| io::Error::other("设备缺少来源"))?;
        if record.settings.trust_sources.get(&device.node_id) != Some(source) {
            settings.trust_sources.insert(
                device.node_id.clone(),
                crate::node::core::authorization::TrustSource::Administrator,
            );
        }
    }
    settings.validate().map_err(io::Error::other)?;
    record.settings = settings;
    record.revision = record
        .revision
        .checked_add(1)
        .ok_or_else(|| io::Error::other("revision已耗尽"))?;
    record.last_request = None;
    record.validate().map_err(io::Error::other)?;
    let outcome = crate::storage::atomic::write_json(&paths.config(), &record)?;
    Ok(json!({"revision":record.revision,"durability_warning":outcome.durability_warning}))
}

impl Agent {
    pub(crate) async fn apply_settings(
        &self,
        expected: u64,
        mut value: Value,
        id: &str,
        digest: &str,
        restart: bool,
    ) -> Result<Value, String> {
        let mut current = self.config.lock().await;
        if current
            .last_request
            .as_ref()
            .is_some_and(|(last, hash)| last == id && hash == digest)
        {
            return Ok(json!({"revision":current.revision,"already_committed":true}));
        }
        if expected != current.revision {
            return Err(format!(
                "revision_conflict: 当前revision为{}",
                current.revision
            ));
        }
        let object = value.as_object_mut().ok_or("配置必须是JSON对象")?;
        if object.contains_key("remote_auth") {
            return Err("密码只能通过专用命令修改，配置草稿不得包含remote_auth".into());
        }
        object.insert(
            "remote_auth".into(),
            serde_json::to_value(&current.settings.remote_auth).map_err(|e| e.to_string())?,
        );
        let settings: NodeSettings = serde_json::from_value(value).map_err(|e| e.to_string())?;
        self.commit_settings(
            &mut current,
            expected,
            settings,
            id,
            digest,
            CommitOptions {
                restart,
                ..Default::default()
            },
        )
        .await
    }
    pub(crate) async fn commit_settings(
        &self,
        current: &mut ConfigRecord,
        expected: u64,
        settings: NodeSettings,
        id: &str,
        digest: &str,
        options: CommitOptions,
    ) -> Result<Value, String> {
        if expected != current.revision {
            return Err(format!(
                "revision_conflict: 当前revision为{}",
                current.revision
            ));
        }
        if let Some(error) = settings.unsupported_capability() {
            return Err(error.into());
        }
        settings.validate().map_err(|e| e.to_string())?;
        // These fields change network/runtime ownership. An online update must
        // never tear down a healthy node merely to learn whether it can start.
        let mut before = serde_json::to_value(&current.settings).map_err(|e| e.to_string())?;
        let mut after = serde_json::to_value(&settings).map_err(|e| e.to_string())?;
        for value in [&mut before, &mut after] {
            if let Some(direct) = value.get_mut("direct").and_then(Value::as_object_mut) {
                direct.remove("peers");
            }
        }
        for key in [
            "resources",
            "services",
            "forwards",
            "service_ids",
            "trusted_devices",
            "trust_sources",
            "remote_auth",
            "send_concurrency",
            "auto_resume",
            "receive_safety_mib",
            "file_limits",
            "receive_root",
        ] {
            before.as_object_mut().unwrap().remove(key);
            after.as_object_mut().unwrap().remove(key);
        }
        if before != after && !options.restart {
            return Err(
                "restart_required: 信令、协议模式、接收根或业务能力修改须停止Agent后离线应用"
                    .into(),
            );
        }
        for device in &settings.trusted_devices {
            let source = settings.trust_sources[&device.node_id];
            if current.settings.trust_sources.get(&device.node_id) != Some(&source)
                && source != crate::node::core::authorization::TrustSource::Administrator
                && !(source == crate::node::core::authorization::TrustSource::PasswordVerified
                    && options
                        .verified
                        .is_some_and(|(peer, _)| peer.to_hex() == device.node_id))
            {
                return Err(
                    "新增或变更授权来源必须显式为administrator；密码证明/旧数据导入有独立入口"
                        .into(),
                );
            }
        }
        let mut forwards = self.forwards.lock().await;
        let prepared = forwarding::prepare(&forwards, &settings, &self.control, None)
            .await
            .map_err(|e| e.to_string())?;
        let record = ConfigRecord {
            schema_version: 1,
            revision: current.revision.checked_add(1).ok_or("revision已耗尽")?,
            last_request: Some((id.into(), digest.into())),
            settings,
        };
        record.validate().map_err(|e| e.to_string())?;
        let policy = record.settings.runtime_policy().map_err(|e| e.to_string())?;
        let path = self.paths.config();
        let commit_record = record.clone();
        let outcome = match self
            .control
            .commit_policy(policy, options, move || {
                crate::storage::atomic::write_json(&path, &commit_record).map_err(|e| e.to_string())
            })
            .await
        {
            Ok(outcome) => outcome,
            Err(error) => {
                if error.starts_with("recovery_required") {
                    self.control.shutdown();
                }
                return Err(error);
            }
        };
        let retired = if options.restart {
            self.restart_pending
                .store(true, std::sync::atomic::Ordering::Release);
            drop(prepared);
            forwards.shutdown().await;
            vec![]
        } else {
            forwards.commit(prepared)
        };
        *current = record;
        if let Err(error) = self.control.configuration_committed(current.revision) {
            self.control.shutdown();
            return Err(format!(
                "recovery_required: 配置已提交但快照不可用：{error}"
            ));
        }
        *self.last_error.lock().await = outcome.durability_warning.clone();
        let result = json!({"revision":current.revision,"durability_warning":outcome.durability_warning,"restart_required":options.restart});
        drop(forwards);
        for task in retired {
            let _ = task.await;
        }
        Ok(result)
    }
}
