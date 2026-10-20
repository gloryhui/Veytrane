//! The node schema is independent of the historical desktop preferences schema.
use super::{
    authorization::{ResourcePolicy, TrustSource},
    config::{AllowedForwardTarget, TunnelRule},
    remote_auth::RemoteVerifier,
    trusted_devices::TrustedDevice,
    wire::PeerWireMode,
};
use crate::{
    error::{Error, Result},
    identity::NodeId,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeSettings {
    pub signal_server: String,
    pub signal_tls: Option<crate::discovery::signal_tls::SignalTlsClient>,
    pub relay_server: Option<String>,
    pub receive_root: PathBuf,
    pub remote_auth: Option<RemoteVerifier>,
    pub trusted_devices: Vec<TrustedDevice>,
    pub trust_sources: HashMap<String, TrustSource>,
    pub peer_modes: HashMap<String, PeerWireMode>,
    pub resources: ResourcePolicy,
    pub services: Vec<AllowedForwardTarget>,
    pub forwards: Vec<TunnelRule>,
    pub service_ids: HashMap<String, String>,
    pub send_concurrency: u8,
    pub auto_resume: bool,
    pub receive_safety_mib: u16,
    pub file_limits: super::bandwidth::FileLimits,
    pub enable_files: bool,
    pub enable_speed: bool,
    pub enable_tcp: bool,
}
impl NodeSettings {
    pub fn initial(receive_root: PathBuf) -> Self {
        Self {
            signal_server: String::new(),
            signal_tls: None,
            relay_server: None,
            receive_root,
            remote_auth: None,
            trusted_devices: vec![],
            trust_sources: HashMap::new(),
            peer_modes: HashMap::new(),
            resources: ResourcePolicy::default(),
            services: vec![],
            forwards: vec![],
            service_ids: HashMap::new(),
            send_concurrency: 1,
            auto_resume: true,
            receive_safety_mib: 64,
            file_limits: Default::default(),
            enable_files: true,
            enable_speed: true,
            enable_tcp: true,
        }
    }
    pub fn validate(&self) -> Result<()> {
        if !self.receive_root.is_absolute() {
            return Err(Error::Protocol("接收根必须是绝对路径".into()));
        }
        if self.signal_server.len() > 512 || self.signal_server.chars().any(char::is_control) {
            return Err(Error::Protocol("信令地址无效".into()));
        }
        if let Some(tls) = &self.signal_tls
            && !self.signal_server.is_empty()
        {
            tls.prepare(&self.signal_server)?;
        }
        if let Some(relay) = &self.relay_server {
            crate::relay::client::validate_server_spec(relay)?;
        }
        if let Some(verifier) = &self.remote_auth {
            verifier.validate()?;
        }
        super::config::validate_send_concurrency(self.send_concurrency)
            .map_err(|e| Error::Protocol(e.to_string()))?;
        super::receive_policy::validate_safety_mib(self.receive_safety_mib)
            .map_err(|e| Error::Protocol(e.to_string()))?;
        self.file_limits
            .validate()
            .map_err(|e| Error::Protocol(e.into()))?;
        if !self.signal_server.is_empty() {
            let (host, port) = self
                .signal_server
                .rsplit_once(':')
                .ok_or_else(|| Error::Protocol("信令地址须为HOST:PORT".into()))?;
            if host.is_empty() || port.parse::<u16>().ok().is_none_or(|p| p == 0) {
                return Err(Error::Protocol("信令地址/端口无效".into()));
            }
            super::config::validate_signal_host(host.trim_start_matches('[').trim_end_matches(']'))
                .map_err(|e| Error::Protocol(e.to_string()))?;
        }
        self.resources.validate()?;
        super::trusted_devices::validate(&self.trusted_devices)
            .map_err(|e| Error::Protocol(e.to_string()))?;
        for (id, mode) in &self.peer_modes {
            if NodeId::from_hex(id)?.to_hex() != *id || *mode == PeerWireMode::LegacyCliV3 {
                return Err(Error::Protocol("设备模式无效或尚不支持旧CLI适配".into()));
            }
        }
        if self.peer_modes.len() > 128 || self.services.len() > 128 || self.forwards.len() > 256 {
            return Err(Error::Protocol("配置条目超出上限".into()));
        }
        for device in &self.trusted_devices {
            if !self.trust_sources.contains_key(&device.node_id) {
                return Err(Error::Protocol("可信设备缺少授权来源".into()));
            }
        }
        if self.trust_sources.len() != self.trusted_devices.len() {
            return Err(Error::Protocol("授权来源包含未授权设备".into()));
        }
        crate::tunnel::rules::ServiceConfig {
            forward_targets: self.services.clone(),
            ..Default::default()
        }
        .validate()?;
        crate::tunnel::rules::ClientConfig {
            rules: self.forwards.clone(),
            ..Default::default()
        }
        .validate()?;
        for service in &self.services {
            super::wire::validate_service_id(&service.id)?;
        }
        let mut listeners = HashSet::new();
        for rule in &self.forwards {
            if !rule.listen.ip().is_loopback() {
                return Err(Error::Protocol("本机监听只允许loopback".into()));
            }
            if rule.enabled && !listeners.insert(rule.listen) {
                return Err(Error::Protocol("监听端口重复".into()));
            }
            let legacy =
                self.peer_modes.get(&rule.peer_node_id) == Some(&PeerWireMode::LegacyDesktopV1);
            if !legacy {
                let id = self
                    .service_ids
                    .get(&rule.id)
                    .ok_or_else(|| Error::Protocol("新协议转发缺少服务ID".into()))?;
                super::wire::validate_service_id(id)?;
            }
        }
        if self
            .service_ids
            .keys()
            .any(|id| !self.forwards.iter().any(|r| &r.id == id))
        {
            return Err(Error::Protocol("服务映射引用不存在的规则".into()));
        }
        Ok(())
    }
    pub fn session(&self) -> Result<super::control::NodeSessionConfig> {
        self.validate()?;
        let mut config = super::control::NodeSessionConfig::unified(&self.signal_server);
        config.signal_tls = self.signal_tls.clone();
        config.network.relay_server = self.relay_server.clone();
        config.remote_auth = self.remote_auth.clone();
        config.trusted_devices = self.trusted_devices.clone();
        config.trust_sources = self
            .trust_sources
            .iter()
            .map(|(id, s)| Ok((NodeId::from_hex(id)?, *s)))
            .collect::<Result<_>>()?;
        config.peer_modes = self
            .peer_modes
            .iter()
            .map(|(id, m)| Ok((NodeId::from_hex(id)?, *m)))
            .collect::<Result<_>>()?;
        config.resources = self.resources.clone();
        config.allowed_forward_targets = self.services.clone();
        config.tunnel_rules = self.forwards.clone();
        config.service_ids = self.service_ids.clone();
        config.enable_files = self.enable_files;
        config.enable_speed = self.enable_speed;
        config.enable_tcp = self.enable_tcp;
        Ok(config)
    }
    pub fn redacted(&self) -> serde_json::Value {
        let mut value = serde_json::to_value(self).expect("settings are JSON serializable");
        value
            .as_object_mut()
            .expect("settings object")
            .remove("remote_auth");
        value
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConfigRecord {
    pub schema_version: u32,
    pub revision: u64,
    pub last_request: Option<(String, String)>,
    pub settings: NodeSettings,
}
impl ConfigRecord {
    pub fn load(path: &Path) -> Result<Self> {
        let metadata = std::fs::symlink_metadata(path)?;
        if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > 1024 * 1024
        {
            return Err(Error::Protocol("节点配置不是有界普通文件".into()));
        }
        let value: Self = serde_json::from_slice(&std::fs::read(path)?)
            .map_err(|e| Error::Protocol(format!("节点配置损坏：{e}")))?;
        if value.schema_version != 1 {
            return Err(Error::Protocol("不支持的节点配置schema".into()));
        }
        value.settings.validate()?;
        Ok(value)
    }
    pub fn initialize(path: &Path, receive_root: PathBuf) -> Result<Self> {
        if path.try_exists()? {
            return Err(Error::Protocol("节点配置已经存在".into()));
        }
        let record = Self {
            schema_version: 1,
            revision: 0,
            last_request: None,
            settings: NodeSettings::initial(receive_root),
        };
        record.settings.validate()?;
        let outcome = crate::storage::atomic::write_json(path, &record)?;
        if let Some(warning) = outcome.durability_warning {
            tracing::warn!(%warning);
        }
        Ok(record)
    }
}
