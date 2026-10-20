//! Persistent TCP forwarding rules shared by the CLI and the desktop app.
//!
//! Two files are supported:
//!
//! - a **service** file authorizes peers to reach specific targets;
//! - a **client** file describes local listeners that share one authenticated
//!   peer session.
//!
//! Both files are deliberately plain JSON so a headless host can edit and
//! `SIGHUP` the process without restarting it. Invalid files never replace the
//! currently active policy.

use std::collections::HashSet;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::{RwLock, RwLockWriteGuard};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::identity::NodeId;

/// Current on-disk schema. Unknown versions are rejected rather than guessed.
pub const RULES_SCHEMA_VERSION: u32 = 1;

fn default_version() -> u32 {
    RULES_SCHEMA_VERSION
}

/// A local TCP service that authenticated peers may reach through this device.
///
/// The desktop settings file stores the same shape. Keep the field names stable:
/// moving the type must not migrate existing `settings.json` values.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AllowedForwardTarget {
    /// Empty grants no access (including migrated schema v2 entries).
    #[serde(default)]
    pub allowed_peers: Vec<String>,
    pub id: String,
    pub name: String,
    pub target: SocketAddr,
    pub enabled: bool,
}

/// A local TCP listener bound to one specific authenticated peer.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TunnelRule {
    pub id: String,
    pub name: String,
    pub peer_node_id: String,
    pub listen: SocketAddr,
    pub target: SocketAddr,
    pub enabled: bool,
    pub auto_start: bool,
}

/// Headless service policy: which peers may reach which targets.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceConfig {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub forward_targets: Vec<AllowedForwardTarget>,
}

impl Default for ServiceConfig {
    fn default() -> Self {
        Self {
            version: RULES_SCHEMA_VERSION,
            forward_targets: Vec::new(),
        }
    }
}

/// Headless client policy: local listeners sharing one peer session.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ClientConfig {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub rules: Vec<TunnelRule>,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            version: RULES_SCHEMA_VERSION,
            rules: Vec::new(),
        }
    }
}

impl ServiceConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let config: Self = load_json(path)?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<()> {
        if self.version != RULES_SCHEMA_VERSION {
            return Err(Error::Protocol(format!(
                "服务配置文件版本 {} 不受支持（当前 {RULES_SCHEMA_VERSION}）",
                self.version
            )));
        }
        validate_forward_targets(&self.forward_targets)
    }

    /// The service currently punches one peer per process. Derive that peer from
    /// the enabled target authorizations and reject ambiguous files early.
    pub fn single_peer(&self) -> Result<NodeId> {
        let peers = self.peers();
        match peers.as_slice() {
            [peer] => Ok(*peer),
            [] => Err(Error::Protocol(
                "服务配置里没有启用的授权目标或授权设备".into(),
            )),
            _ => Err(Error::Protocol(format!(
                "serve 目前每个进程只支持一个对端，配置里出现了 {} 个授权设备；请拆分配置或改用桌面端",
                peers.len()
            ))),
        }
    }

    pub fn peers(&self) -> Vec<NodeId> {
        let mut peers = Vec::new();
        for target in self.forward_targets.iter().filter(|entry| entry.enabled) {
            for peer in &target.allowed_peers {
                if let Ok(peer) = NodeId::from_hex(peer)
                    && !peers.contains(&peer)
                {
                    peers.push(peer);
                }
            }
        }
        peers
    }
}

impl ClientConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let config: Self = load_json(path)?;
        config.validate()?;
        Ok(config)
    }

    pub fn validate(&self) -> Result<()> {
        if self.version != RULES_SCHEMA_VERSION {
            return Err(Error::Protocol(format!(
                "客户端配置文件版本 {} 不受支持（当前 {RULES_SCHEMA_VERSION}）",
                self.version
            )));
        }
        validate_client_rules(&self.rules)
    }

    /// All enabled rules must share one peer so a single authenticated session
    /// can serve every listener.
    pub fn single_peer(&self) -> Result<NodeId> {
        self.validate()?;
        match self.peers().as_slice() {
            [peer] => Ok(*peer),
            [] => Err(Error::Protocol("客户端配置里没有启用的转发规则".into())),
            _ => Err(Error::Protocol(format!(
                "所有启用规则必须共享同一个对端，当前出现了 {} 个对端",
                self.peers().len()
            ))),
        }
    }

    /// Unique peers referenced by enabled rules.
    pub fn peers(&self) -> Vec<NodeId> {
        let mut peers = Vec::new();
        for rule in self.rules.iter().filter(|rule| rule.enabled) {
            if let Ok(peer) = NodeId::from_hex(&rule.peer_node_id)
                && !peers.contains(&peer)
            {
                peers.push(peer);
            }
        }
        peers
    }

    pub fn enabled_rules(&self) -> Vec<TunnelRule> {
        self.rules
            .iter()
            .filter(|rule| rule.enabled)
            .cloned()
            .collect()
    }
}

fn load_json<T: DeserializeOwned>(path: &Path) -> Result<T> {
    let text = std::fs::read_to_string(path).map_err(|error| {
        Error::Io(std::io::Error::new(
            error.kind(),
            format!("读取配置文件 {} 失败: {error}", path.display()),
        ))
    })?;
    serde_json::from_str(&text).map_err(|error| {
        Error::Protocol(format!(
            "解析配置文件 {} 失败（第 {} 行第 {} 列）: {error}",
            path.display(),
            error.line(),
            error.column()
        ))
    })
}

fn validate_forward_targets(targets: &[AllowedForwardTarget]) -> Result<()> {
    let mut ids = HashSet::new();
    let mut enabled_targets = HashSet::new();
    for entry in targets {
        validate_rule_id(&entry.id, "允许服务规则")?;
        validate_rule_name(&entry.name, "允许服务规则")?;
        validate_target(entry.target)?;
        let mut peers = HashSet::new();
        for peer in &entry.allowed_peers {
            let peer = NodeId::from_hex(peer)
                .map_err(|error| Error::Protocol(format!("授权设备 ID 无效：{error}")))?;
            if !peers.insert(peer) {
                return Err(Error::Protocol(format!(
                    "允许服务规则 {} 的授权设备 ID 重复",
                    entry.id
                )));
            }
        }
        if !ids.insert(entry.id.as_str()) {
            return Err(Error::Protocol(format!(
                "允许服务规则 ID 重复：{}",
                entry.id
            )));
        }
        if entry.enabled && !enabled_targets.insert(entry.target) {
            return Err(Error::Protocol(format!(
                "已启用的允许服务目标重复：{}",
                entry.target
            )));
        }
    }
    Ok(())
}

fn validate_client_rules(rules: &[TunnelRule]) -> Result<()> {
    let mut ids = HashSet::new();
    let mut listen = HashSet::new();
    for rule in rules {
        validate_rule_id(&rule.id, "本机转发规则")?;
        validate_rule_name(&rule.name, "本机转发规则")?;
        NodeId::from_hex(&rule.peer_node_id)
            .map_err(|error| Error::Protocol(format!("对端 Node ID 无效：{error}")))?;
        if !rule.listen.ip().is_loopback() || rule.listen.port() == 0 {
            return Err(Error::Protocol(format!(
                "本机监听地址 {} 必须是 127.0.0.1 或 ::1 且端口在 1..65535 内",
                rule.listen
            )));
        }
        validate_target(rule.target)?;
        if !ids.insert(rule.id.as_str()) {
            return Err(Error::Protocol(format!(
                "本机转发规则 ID 重复：{}",
                rule.id
            )));
        }
        if rule.enabled && !listen.insert(rule.listen) {
            return Err(Error::Protocol(format!(
                "已启用的本机监听地址重复：{}",
                rule.listen
            )));
        }
    }
    Ok(())
}

fn validate_rule_id(id: &str, kind: &str) -> Result<()> {
    if id.is_empty()
        || id.len() > 64
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(Error::Protocol(format!(
            "{kind} ID 必须为 1..=64 个 ASCII 字母、数字、`-`、`_` 或 `.`，收到 {id:?}"
        )));
    }
    Ok(())
}

fn validate_rule_name(name: &str, kind: &str) -> Result<()> {
    if name.trim().is_empty() || name.chars().count() > 128 {
        return Err(Error::Protocol(format!(
            "{kind} 名称必须为 1..=128 个字符，收到 {name:?}"
        )));
    }
    if name.chars().any(char::is_control) {
        return Err(Error::Protocol(format!("{kind} 名称不能包含控制字符")));
    }
    Ok(())
}

fn validate_target(target: SocketAddr) -> Result<()> {
    if target.port() == 0 || target.ip().is_unspecified() || target.ip().is_multicast() {
        return Err(Error::Protocol(format!(
            "目标地址 {target} 必须使用具体 IP 和 1..65535 端口"
        )));
    }
    Ok(())
}

/// Runtime service policy. The active snapshot is replaced atomically; readers
/// never observe a half-validated file.
#[derive(Debug)]
pub struct ServePolicy {
    snapshot: RwLock<CompiledPolicy>,
}

#[derive(Clone, Default, Debug)]
struct CompiledPolicy {
    targets: Vec<CompiledTarget>,
    peers: Vec<NodeId>,
}

#[derive(Clone, Debug)]
struct CompiledTarget {
    target: SocketAddr,
    peers: HashSet<NodeId>,
}

pub(crate) struct PreparedPolicy<'a> {
    guard: RwLockWriteGuard<'a, CompiledPolicy>,
    next: CompiledPolicy,
}

impl PreparedPolicy<'_> {
    pub(crate) fn commit(mut self) {
        *self.guard = self.next;
    }
}

impl ServePolicy {
    pub fn new(config: &ServiceConfig) -> Result<Self> {
        config.validate()?;
        Ok(Self {
            snapshot: RwLock::new(compile(config)),
        })
    }

    /// Validate first, then swap. A failed reload leaves the previous policy
    /// untouched.
    pub fn replace(&self, config: &ServiceConfig) -> Result<()> {
        self.prepare_replace(config)?.commit();
        Ok(())
    }

    pub(crate) fn prepare_replace(&self, config: &ServiceConfig) -> Result<PreparedPolicy<'_>> {
        config.validate()?;
        let compiled = compile(config);
        let guard = self
            .snapshot
            .write()
            .map_err(|_| Error::Protocol("服务策略锁已损坏".into()))?;
        Ok(PreparedPolicy {
            guard,
            next: compiled,
        })
    }

    pub fn peer_allowed(&self, peer: NodeId) -> bool {
        self.snapshot
            .read()
            .map(|policy| policy.peers.contains(&peer))
            .unwrap_or(false)
    }

    pub fn target_allowed(&self, peer: NodeId, target: SocketAddr) -> bool {
        self.snapshot
            .read()
            .map(|policy| {
                policy
                    .targets
                    .iter()
                    .any(|entry| entry.target == target && entry.peers.contains(&peer))
            })
            .unwrap_or(false)
    }

    pub fn targets_for(&self, peer: NodeId) -> Vec<SocketAddr> {
        self.snapshot
            .read()
            .map(|policy| {
                policy
                    .targets
                    .iter()
                    .filter(|entry| entry.peers.contains(&peer))
                    .map(|entry| entry.target)
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn peers(&self) -> Vec<NodeId> {
        self.snapshot
            .read()
            .map(|policy| policy.peers.clone())
            .unwrap_or_default()
    }
}

fn compile(config: &ServiceConfig) -> CompiledPolicy {
    let mut targets = Vec::new();
    let mut peers = Vec::new();
    for entry in config.forward_targets.iter().filter(|entry| entry.enabled) {
        let allowed: HashSet<NodeId> = entry
            .allowed_peers
            .iter()
            .filter_map(|peer| NodeId::from_hex(peer).ok())
            .collect();
        for peer in &allowed {
            if !peers.contains(peer) {
                peers.push(*peer);
            }
        }
        targets.push(CompiledTarget {
            target: entry.target,
            peers: allowed,
        });
    }
    CompiledPolicy { targets, peers }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node_id(seed: u8) -> NodeId {
        NodeId::from_hex(&format!("{seed:02x}").repeat(16)).unwrap()
    }

    fn service_config(peer: NodeId, target: &str) -> ServiceConfig {
        ServiceConfig {
            version: RULES_SCHEMA_VERSION,
            forward_targets: vec![AllowedForwardTarget {
                allowed_peers: vec![peer.to_hex()],
                id: "sub2api".into(),
                name: "Sub2API".into(),
                target: target.parse().unwrap(),
                enabled: true,
            }],
        }
    }

    #[test]
    fn service_policy_is_atomic_and_per_peer() {
        let peer = node_id(1);
        let other = node_id(2);
        let policy = ServePolicy::new(&service_config(peer, "127.0.0.1:8080")).unwrap();
        assert!(policy.peer_allowed(peer));
        assert!(!policy.peer_allowed(other));
        assert!(policy.target_allowed(peer, "127.0.0.1:8080".parse().unwrap()));
        assert!(!policy.target_allowed(peer, "127.0.0.1:8081".parse().unwrap()));

        let mut invalid = service_config(peer, "127.0.0.1:8080");
        invalid.version = 99;
        assert!(policy.replace(&invalid).is_err());
        // The failed replacement keeps the old target.
        assert!(policy.target_allowed(peer, "127.0.0.1:8080".parse().unwrap()));

        let mut replacement = service_config(peer, "127.0.0.1:9090");
        replacement.forward_targets[0].allowed_peers = vec![peer.to_hex(), other.to_hex()];
        policy.replace(&replacement).unwrap();
        assert!(policy.peer_allowed(other));
        assert!(policy.target_allowed(other, "127.0.0.1:9090".parse().unwrap()));
        assert!(!policy.target_allowed(peer, "127.0.0.1:8080".parse().unwrap()));
    }

    #[test]
    fn service_config_rejects_duplicate_peers_and_targets() {
        let peer = node_id(1);
        let mut config = service_config(peer, "127.0.0.1:8080");
        config.forward_targets[0].allowed_peers.push(peer.to_hex());
        assert!(config.validate().is_err());

        let mut duplicate = service_config(peer, "127.0.0.1:8080");
        let mut second = duplicate.forward_targets[0].clone();
        second.id = "second".into();
        duplicate.forward_targets.push(second);
        assert!(duplicate.validate().is_err());
    }

    #[test]
    fn client_rules_require_one_peer_and_loopback_listeners() {
        let peer = node_id(3);
        let rule = TunnelRule {
            id: "api".into(),
            name: "Sub2API".into(),
            peer_node_id: peer.to_hex(),
            listen: "127.0.0.1:18080".parse().unwrap(),
            target: "127.0.0.1:8080".parse().unwrap(),
            enabled: true,
            auto_start: true,
        };
        let config = ClientConfig {
            version: RULES_SCHEMA_VERSION,
            rules: vec![rule.clone()],
        };
        assert_eq!(config.single_peer().unwrap(), peer);

        let mut wildcard = config.clone();
        wildcard.rules[0].listen = "0.0.0.0:18080".parse().unwrap();
        assert!(wildcard.validate().is_err());

        let mut duplicate = config.clone();
        duplicate.rules.push(TunnelRule {
            id: "second".into(),
            name: "second".into(),
            ..rule.clone()
        });
        assert!(duplicate.validate().is_err());

        let mut two_peers = config.clone();
        two_peers.rules.push(TunnelRule {
            id: "second".into(),
            name: "second".into(),
            listen: "127.0.0.1:18081".parse().unwrap(),
            peer_node_id: node_id(4).to_hex(),
            ..rule
        });
        assert!(two_peers.single_peer().is_err());
    }

    #[test]
    fn missing_or_broken_files_are_reported_with_context() {
        let path =
            std::env::temp_dir().join(format!("p2p-rules-{}-missing.json", std::process::id()));
        let error = ServiceConfig::load(&path).unwrap_err().to_string();
        assert!(error.contains("读取配置文件"), "{error}");
    }

    #[test]
    fn documented_examples_stay_valid() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/examples");
        ServiceConfig::load(&root.join("service.json")).unwrap();
        ClientConfig::load(&root.join("client.json")).unwrap();
    }
}
