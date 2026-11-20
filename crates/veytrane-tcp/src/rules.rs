//! Shared TCP forwarding rules and validation.

use std::{collections::HashSet, net::SocketAddr};

use serde::{Deserialize, Serialize};
use veytrane_transport::{
    error::{Error, Result},
    identity::NodeId,
};

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

impl AllowedForwardTarget {
    pub fn new(name: impl Into<String>, target: SocketAddr, allowed_peers: Vec<String>) -> Self {
        Self {
            id: new_rule_id(),
            name: name.into(),
            allowed_peers,
            target,
            enabled: true,
        }
    }
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

impl TunnelRule {
    pub fn new(
        name: impl Into<String>,
        peer_node_id: impl Into<String>,
        listen_port: u16,
        target: SocketAddr,
    ) -> Self {
        Self {
            id: new_rule_id(),
            name: name.into(),
            peer_node_id: peer_node_id.into(),
            listen: SocketAddr::from(([127, 0, 0, 1], listen_port)),
            target,
            enabled: true,
            auto_start: false,
        }
    }
}

fn new_rule_id() -> String {
    format!("{:032x}", rand::random::<u128>())
}

pub fn validate_forward_targets(targets: &[AllowedForwardTarget]) -> Result<()> {
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

pub fn validate_client_rules(rules: &[TunnelRule]) -> Result<()> {
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
