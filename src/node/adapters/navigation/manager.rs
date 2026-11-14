//! Navigation presentation and revisioned management facade.
//!
//! This module never owns a forwarding runtime. Every networking mutation is
//! delegated through `Backend` to the running Agent's IPC command surface.

use super::{Backend, Problem, Result, address, id, stamp, write_private, write_private_bytes};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    net::{SocketAddr, TcpListener},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const DATA_OPEN: &str = r#"<script id="navigation-data" type="application/json">"#;
const DATA_CLOSE: &str = "</script>";
const PLAN_TTL: Duration = Duration::from_secs(600);
const MAX_PLANS: usize = 32;
const MAX_RECEIPTS: usize = 300;

#[derive(Clone)]
struct Plan {
    expires: Instant,
    config: Value,
    settings: Value,
    remote_before: Option<Value>,
    remote_after: Option<Value>,
    rule: Value,
    previous: Option<Value>,
    meta: Option<Value>,
}

pub struct Manager {
    config: Value,
    host: Value,
    bridge: Arc<dyn Backend>,
    page: PathBuf,
    receipts_path: PathBuf,
    meta_path: PathBuf,
    receipts: Map<String, Value>,
    metadata: Map<String, Value>,
    plans: BTreeMap<String, Plan>,
    remote: Option<Value>,
    remote_error: Option<String>,
    local_name: String,
    remote_name: String,
}

impl Manager {
    pub fn new(config: Value, host: Value, bridge: Arc<dyn Backend>) -> Result<Self> {
        let page = config_path(&config, "page")?;
        let state_dir = config_path(&config, "state_dir")?;
        let receipts_path = state_dir.join("operations.json");
        let meta_path = state_dir.join("presentation.json");
        let receipts = read_map(&receipts_path, "operations.json")?;
        let metadata = read_map(&meta_path, "presentation.json")?;
        let local_name = required_string(&host, "name", "host.name")?.to_owned();
        if required_string(&host, "localAdminId", "host.localAdminId")?.is_empty()
            || required_string(&host, "adminUrl", "host.adminUrl")?.is_empty()
        {
            return Err(Problem::new("导航 host 信息不完整。"));
        }
        let remote_name = config
            .get("remote")
            .and_then(Value::as_object)
            .and_then(|remote| remote.get("name"))
            .and_then(Value::as_str)
            .unwrap_or("远端设备")
            .to_owned();
        Ok(Self {
            config,
            host,
            bridge,
            page,
            receipts_path,
            meta_path,
            receipts,
            metadata,
            plans: BTreeMap::new(),
            remote: None,
            remote_error: None,
            local_name,
            remote_name,
        })
    }

    pub async fn state(&mut self, refresh_remote: bool) -> Result<Value> {
        let mut errors = Vec::<String>::new();
        let config = match self.local_config().await {
            Ok(config) => config,
            Err(error) => {
                return Ok(json!({
                    "checkedAt": stamp(),
                    "navigation": self.baseline()?,
                    "host": self.host,
                    "agent": {"available": false},
                    "errors": [error.message],
                    "history": self.history(),
                }));
            }
        };
        let mut status = match self
            .backend_call(json!({"method":"status"}), false, None)
            .await
        {
            Ok(status) => status,
            Err(error) => {
                errors.push(error.message);
                json!({})
            }
        };
        if status.is_null() {
            status = json!({});
        }
        if refresh_remote
            && self
                .config
                .get("remote")
                .is_some_and(|value| !value.is_null())
        {
            if let Err(error) = self.remote_config().await {
                errors.push(format!(
                    "{} 独立管理通道：{}",
                    self.remote_name, error.message
                ));
            }
        }
        let settings = required_object(&config, "settings", "config.settings")?.clone();
        let managed = match self.managed_peer(&Value::Object(settings.clone()), false) {
            Ok(peer) => peer,
            Err(error) => {
                errors.push(error.message);
                None
            }
        };
        let baseline = self.baseline()?;
        let known_peers = known_peers(&Value::Object(settings.clone()))?;
        let status_peers = status
            .get("node")
            .and_then(Value::as_object)
            .and_then(|node| node.get("peers"))
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let mut nodes = Vec::new();
        for peer in known_peers {
            let peer_state = status_peers.get(&peer).cloned().unwrap_or(json!({}));
            let mappings = settings
                .get("forwards")
                .and_then(Value::as_array)
                .map(|forwards| {
                    forwards
                        .iter()
                        .filter(|rule| {
                            rule.get("peer_node_id").and_then(Value::as_str) == Some(&peer)
                        })
                        .count()
                })
                .unwrap_or(0);
            let transport = peer_state.get("transport");
            let rtt_ms = transport.and_then(Value::as_object).map(|_| {
                let micros = transport
                    .and_then(|value| value.get("rtt_us"))
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                python_round_1(micros as f64 / 1000.0)
            });
            nodes.push(json!({
                "id": peer,
                "label": self.peer_label(&peer, &Value::Object(settings.clone()), &baseline),
                "phase": peer_state.get("phase").and_then(Value::as_str).unwrap_or("unknown"),
                "rttMs": rtt_ms,
                "editableTarget": managed.as_deref() == Some(peer.as_str()),
                "mappings": mappings,
            }));
        }
        let available = status.as_object().is_some_and(|value| !value.is_empty());
        let agent = json!({
            "available": available,
            "running": status.get("core_running").and_then(Value::as_bool).unwrap_or(false),
            "epoch": status.get("epoch").cloned().unwrap_or(Value::Null),
            "activeStreams": status.pointer("/forwards/active_streams").and_then(Value::as_u64).unwrap_or(0),
            "lastError": status.get("last_error").cloned().unwrap_or(Value::Null),
            "diagnostics": status.get("diagnostics").cloned().unwrap_or_else(|| json!([])),
            "signal": status.pointer("/node/signal").cloned().unwrap_or(Value::Null),
        });
        Ok(json!({
            "checkedAt": stamp(),
            "host": self.host,
            "navigation": self.navigation(&config, &baseline)?,
            "revision": required_u64(&config, "revision", "config.revision")?,
            "agent": agent,
            "rules": status.pointer("/forwards/rules").cloned().unwrap_or_else(|| json!([])),
            "nodes": nodes,
            "remoteConfigAvailable": self.remote_error.is_none() && self.remote.is_some(),
            "errors": errors,
            "history": self.history(),
        }))
    }

    pub async fn catalog(&mut self, peer: &str) -> Result<Value> {
        let config = self.local_config().await?;
        let settings = required_object(&config, "settings", "config.settings")?;
        let managed = self.managed_peer(&Value::Object(settings.clone()), false)?;
        if !known_peers(&Value::Object(settings.clone()))?.contains(peer) {
            return Err(Problem::new("请选择已配对或已有映射的设备。"));
        }
        if managed.as_deref() == Some(peer) {
            let remote = self.remote_config().await?;
            let status = self
                .backend_call(json!({"method":"status"}), false, None)
                .await?;
            let local_node = nested_string(&status, &["node", "node_id"], "status.node.node_id")?;
            let services = remote
                .pointer("/settings/services")
                .and_then(Value::as_array)
                .ok_or_else(|| Problem::new("远端配置缺少服务列表。"))?;
            let mut result = Vec::new();
            for service in services {
                let mut service = required_object_value(service, "remote service")?.clone();
                let authorized = service
                    .get("allowed_peers")
                    .and_then(Value::as_array)
                    .is_some_and(|peers| {
                        peers.iter().any(|item| item.as_str() == Some(&local_node))
                    });
                service.insert("authorized".into(), Value::Bool(authorized));
                result.push(Value::Object(service));
            }
            return Ok(json!({
                "editableTarget": true,
                "services": result,
                "revision": required_u64(&remote, "revision", "remote.revision")?,
            }));
        }
        let services = self
            .backend_call(json!({"method":"services","peer":peer}), false, None)
            .await?;
        Ok(json!({"editableTarget": false, "services": services}))
    }

    pub async fn preview(&mut self, form: &Value) -> Result<Value> {
        self.prune_plans();
        if self.plans.len() >= MAX_PLANS {
            return Err(Problem::new("待确认草稿过多，请稍后重试。"));
        }
        let form = form
            .as_object()
            .ok_or_else(|| Problem::new("请求字段无效，请重新读取配置。"))?;
        let config = self.local_config().await?;
        let settings = required_object(&config, "settings", "config.settings")?.clone();
        let mut settings_value = Value::Object(settings);
        let existing_id = optional_string(form.get("id"));
        let previous = existing_id.as_deref().and_then(|id| {
            settings_value
                .get("forwards")
                .and_then(Value::as_array)
                .and_then(|forwards| {
                    forwards
                        .iter()
                        .find(|rule| rule.get("id").and_then(Value::as_str) == Some(id))
                        .cloned()
                })
        });
        if existing_id.is_some() && previous.is_none() {
            return Err(Problem::code(
                "映射已被其他入口移除，请刷新后重试。",
                "revision_conflict",
            ));
        }
        if form.get("remove").and_then(Value::as_bool) == Some(true) {
            let Some(previous_rule) = previous.clone() else {
                return Err(Problem::new("不能移除远端管理入口或不存在的映射。"));
            };
            let rule_id = existing_id
                .as_deref()
                .ok_or_else(|| Problem::new("不能移除远端管理入口或不存在的映射。"))?;
            let service_id = settings_value
                .get("service_ids")
                .and_then(Value::as_object)
                .and_then(|ids| ids.get(rule_id))
                .and_then(Value::as_str);
            if self.protected_service().as_deref() == service_id {
                return Err(Problem::new("不能移除远端管理入口或不存在的映射。"));
            }
            let forwards = settings_value
                .get_mut("forwards")
                .and_then(Value::as_array_mut)
                .ok_or_else(|| Problem::new("配置缺少转发列表。"))?;
            forwards.retain(|rule| rule.get("id").and_then(Value::as_str) != Some(rule_id));
            settings_value
                .get_mut("service_ids")
                .and_then(Value::as_object_mut)
                .ok_or_else(|| Problem::new("配置缺少服务映射。"))?
                .remove(rule_id);
            let token = id();
            self.plans.insert(
                token.clone(),
                Plan {
                    expires: Instant::now() + PLAN_TTL,
                    config: config.clone(),
                    settings: settings_value,
                    remote_before: None,
                    remote_after: None,
                    rule: previous_rule.clone(),
                    previous: Some(previous_rule.clone()),
                    meta: None,
                },
            );
            return Ok(json!({
                "plan": token,
                "changes": [format!(
                    "{}：移除 {}（{}）的映射。",
                    self.local_name,
                    string_field(&previous_rule, "name").unwrap_or(""),
                    string_field(&previous_rule, "listen").unwrap_or(""),
                )],
                "warnings": [format!("仅移除这条 {} 转发；远端服务和其他映射保留。", self.local_name)],
                "ruleId": rule_id,
                "localRevision": required_u64(&config, "revision", "config.revision")?,
                "remoteRevision": null,
            }));
        }

        let name = form_text(form.get("name")).trim().to_owned();
        let peer = form_text(form.get("peer")).trim().to_owned();
        let sid = form_text(form.get("serviceId")).trim().to_owned();
        if name.is_empty()
            || name.chars().count() > 128
            || !is_lower_hex(&peer, 32)
            || sid.is_empty()
            || sid.len() > 64
            || !sid
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'))
        {
            return Err(Problem::new(
                "请填写名称、已有设备和有效服务 ID（字母、数字、点、横线、下划线）。",
            ));
        }
        let settings_for_peers = settings_value.clone();
        if !known_peers(&settings_for_peers)?.contains(&peer) {
            return Err(Problem::new("请选择已配对或已有映射的设备。"));
        }
        let managed = self.managed_peer(&settings_for_peers, false)?;
        if self.protected_service().as_deref() == Some(sid.as_str())
            && managed.as_deref() != Some(peer.as_str())
        {
            return Err(Problem::new("管理服务 ID 只能用于配置的原远端节点。"));
        }
        let listen = address(
            form.get("listen")
                .and_then(Value::as_str)
                .ok_or_else(|| Problem::new("请填写有效的本机回环监听地址。"))?,
            true,
        )?;
        let listen_text = listen.to_string();
        let enabled = form
            .get("enabled")
            .and_then(Value::as_bool)
            .ok_or_else(|| Problem::new("启用和自动启动必须是布尔值。"))?;
        let auto_start = form
            .get("autoStart")
            .and_then(Value::as_bool)
            .ok_or_else(|| Problem::new("启用和自动启动必须是布尔值。"))?;
        let mut rule = previous
            .clone()
            .unwrap_or_else(|| json!({"id":format!("nav-{}", id()), "target":"127.0.0.1:1"}));
        {
            let rule_object = rule
                .as_object_mut()
                .ok_or_else(|| Problem::new("已有映射格式无效。"))?;
            rule_object.insert("name".into(), json!(name));
            rule_object.insert("peer_node_id".into(), json!(peer));
            rule_object.insert("listen".into(), json!(listen_text));
            rule_object.insert("enabled".into(), json!(enabled));
            rule_object.insert("auto_start".into(), json!(auto_start));
        }
        self.check_port(&settings_value, &rule, previous.as_ref())?;
        let old_sid = existing_id.as_deref().and_then(|rule_id| {
            settings_for_peers
                .get("service_ids")
                .and_then(Value::as_object)
                .and_then(|ids| ids.get(rule_id))
                .and_then(Value::as_str)
        });
        if self.protected_service().as_deref() == old_sid
            && (sid != old_sid.unwrap_or_default()
                || previous
                    .as_ref()
                    .and_then(|value| value.get("peer_node_id"))
                    .and_then(Value::as_str)
                    != Some(peer.as_str()))
        {
            return Err(Problem::new("请保留远端管理服务的设备与服务 ID。"));
        }
        let protocol = form
            .get("protocol")
            .and_then(Value::as_str)
            .unwrap_or("tcp")
            .to_owned();
        let mut path = form_text(form.get("path")).trim().to_owned();
        if path.is_empty() {
            path = "/".into();
        }
        let health_path = form_text(form.get("healthPath")).trim().to_owned();
        let category = form
            .get("category")
            .and_then(Value::as_str)
            .unwrap_or("其他")
            .to_owned();
        if !matches!(
            protocol.as_str(),
            "http" | "https" | "tcp" | "ssh" | "smb" | "rdp"
        ) || !matches!(
            category.as_str(),
            "节点管理" | "路由与网络" | "应用" | "文件" | "其他"
        ) {
            return Err(Problem::new("访问协议或分组不受支持。"));
        }
        for value in [&path, &health_path] {
            if !value.is_empty()
                && (!value.starts_with('/')
                    || value.starts_with("//")
                    || value.len() > 512
                    || !value.bytes().all(|byte| (32..=126).contains(&byte))
                    || value
                        .chars()
                        .any(|character| matches!(character, '\\' | '?' | '#')))
            {
                return Err(Problem::new(
                    "路径应为 / 开头的 ASCII 路径，不含域名、查询参数或凭据。",
                ));
            }
        }
        let meta = json!({
            "peer": peer,
            "serviceId": sid,
            "title": name,
            "protocol": protocol,
            "path": path,
            "category": category,
            "healthPath": health_path,
        });
        let mut remote_before = None;
        let mut remote_after = None;
        let mut changes = Vec::<String>::new();
        let mut warnings = Vec::<String>::new();
        if managed.as_deref() == Some(peer.as_str()) {
            let before = self.remote_config().await?;
            let mut after = before
                .get("settings")
                .cloned()
                .ok_or_else(|| Problem::new("远端配置缺少 settings。"))?;
            let target = form_text(form.get("target")).trim().to_owned();
            let target = if target.is_empty() {
                None
            } else {
                Some(address(&target, false)?.to_string())
            };
            let services = after
                .get_mut("services")
                .and_then(Value::as_array_mut)
                .ok_or_else(|| Problem::new("远端配置缺少服务列表。"))?;
            let service_index = services
                .iter()
                .position(|service| service.get("id").and_then(Value::as_str) == Some(&sid));
            let local_node = self
                .backend_call(json!({"method":"status"}), false, None)
                .await?
                .pointer("/node/node_id")
                .and_then(Value::as_str)
                .ok_or_else(|| Problem::new("本机状态缺少节点 ID。"))?
                .to_owned();
            if let Some(index) = service_index {
                let service = services[index]
                    .as_object_mut()
                    .ok_or_else(|| Problem::new("远端服务格式无效。"))?;
                let authorized = service
                    .get("allowed_peers")
                    .and_then(Value::as_array)
                    .is_some_and(|peers| {
                        peers.iter().any(|peer| peer.as_str() == Some(&local_node))
                    });
                if !authorized {
                    return Err(Problem::new(format!(
                        "{} 服务尚未授权这台 {}，请在节点管理中核实授权。",
                        self.remote_name, self.local_name
                    )));
                }
                if service.get("enabled").and_then(Value::as_bool) != Some(true) {
                    return Err(Problem::new(format!(
                        "{} 服务已禁用，请先在节点管理中启用。",
                        self.remote_name
                    )));
                }
                if let Some(target) = target
                    && service.get("target").and_then(Value::as_str) != Some(&target)
                {
                    if self.protected_service().as_deref() == Some(sid.as_str()) {
                        return Err(Problem::new("远端节点管理目标请在原节点管理页修改。"));
                    }
                    let old_target = service
                        .get("target")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_owned();
                    changes.push(format!(
                        "{}：{} 的目标 {} → {}。",
                        self.remote_name, sid, old_target, target
                    ));
                    let count = service
                        .get("allowed_peers")
                        .and_then(Value::as_array)
                        .map(Vec::len)
                        .unwrap_or(0);
                    warnings.push(format!(
                        "这是共享服务目标，将影响获授权的 {} 台设备之后建立的连接。",
                        count
                    ));
                    service.insert("target".into(), json!(target));
                }
            } else {
                let target =
                    target.ok_or_else(|| Problem::new("创建远端服务时请填写目标 IP:端口。"))?;
                let allowed_peers = json!([local_node]);
                services.push(json!({
                    "id": sid,
                    "name": name,
                    "target": target,
                    "enabled": true,
                    "allowed_peers": allowed_peers,
                }));
                changes.push(format!(
                    "{}：创建服务 {} → {}，仅授权这台 {}。",
                    self.remote_name, sid, target, self.local_name
                ));
            }
            remote_before = Some(before);
            remote_after = Some(after);
        } else {
            let catalog = self
                .backend_call(json!({"method":"services","peer":peer}), false, None)
                .await?;
            let catalog = catalog.get("services").cloned().unwrap_or(catalog);
            let present = catalog.as_array().is_some_and(|services| {
                services
                    .iter()
                    .any(|service| service.get("id").and_then(Value::as_str) == Some(&sid))
            });
            if !present {
                return Err(Problem::new(
                    "服务未出现在该设备的授权目录中。请先在远端发布并授权。",
                ));
            }
            if form.get("target").is_some_and(truthy) {
                return Err(Problem::new(
                    "此设备暂不支持从本页修改服务目标，请选择已授权服务。",
                ));
            }
        }
        if let Some(previous) = &previous {
            let rule_id = existing_id.as_deref().unwrap_or_default();
            let forwards = settings_value
                .get_mut("forwards")
                .and_then(Value::as_array_mut)
                .ok_or_else(|| Problem::new("配置缺少转发列表。"))?;
            for current in forwards.iter_mut() {
                if current.get("id").and_then(Value::as_str) == Some(rule_id) {
                    *current = rule.clone();
                }
            }
            for (field, label) in [
                ("name", "名称"),
                ("listen", "本机监听"),
                ("peer_node_id", "远端设备"),
                ("enabled", "启用"),
                ("auto_start", "自动启动"),
            ] {
                if previous.get(field) != rule.get(field) {
                    changes.push(format!(
                        "{} {}：{} → {}。",
                        self.local_name,
                        label,
                        python_text(previous.get(field).unwrap_or(&Value::Null)),
                        python_text(rule.get(field).unwrap_or(&Value::Null)),
                    ));
                }
            }
            if old_sid != Some(sid.as_str()) {
                changes.push(format!(
                    "{} 服务：{} → {}。",
                    self.local_name,
                    old_sid.unwrap_or(""),
                    sid
                ));
            }
        } else {
            settings_value
                .get_mut("forwards")
                .and_then(Value::as_array_mut)
                .ok_or_else(|| Problem::new("配置缺少转发列表。"))?
                .push(rule.clone());
            changes.push(format!(
                "{}：新增 {} → {}。",
                self.local_name,
                string_field(&rule, "listen").unwrap_or(""),
                sid
            ));
        }
        settings_value
            .get_mut("service_ids")
            .and_then(Value::as_object_mut)
            .ok_or_else(|| Problem::new("配置缺少服务映射。"))?
            .insert(
                string_field(&rule, "id").unwrap_or_default().to_owned(),
                json!(sid),
            );
        if let Some(previous) = &previous
            && (previous.get("listen") != rule.get("listen")
                || previous.get("peer_node_id") != rule.get("peer_node_id")
                || old_sid != Some(sid.as_str())
                || rule.get("enabled").and_then(Value::as_bool) != Some(true))
        {
            warnings.push("本条规则的新连接会使用新设置；其他未修改的映射保留。".into());
        }
        if changes.is_empty() {
            changes.push("仅更新导航展示设置；Agent 配置保持当前版本。".into());
        }
        self.prune_plans();
        if self.plans.len() >= MAX_PLANS {
            return Err(Problem::new("待确认草稿过多，请稍后重试。"));
        }
        let token = id();
        let rule_id = string_field(&rule, "id")
            .ok_or_else(|| Problem::new("映射缺少规则 ID。"))?
            .to_owned();
        let local_revision = required_u64(&config, "revision", "config.revision")?;
        let remote_revision = remote_before
            .as_ref()
            .map(|value| required_u64(value, "revision", "remote.revision"))
            .transpose()?;
        self.plans.insert(
            token.clone(),
            Plan {
                expires: Instant::now() + PLAN_TTL,
                config,
                settings: settings_value,
                remote_before,
                remote_after,
                rule,
                previous,
                meta: Some(meta),
            },
        );
        Ok(json!({
            "plan": token,
            "changes": changes,
            "warnings": warnings,
            "ruleId": rule_id,
            "localRevision": local_revision,
            "remoteRevision": remote_revision,
        }))
    }

    pub async fn mutate(&mut self, action: &str, body: &Value) -> Result<Value> {
        let body_object = body
            .as_object()
            .ok_or_else(|| Problem::new("请求字段无效，请重新读取配置。"))?;
        let operation = body_object
            .get("operation")
            .and_then(Value::as_str)
            .filter(|value| is_lower_hex(value, 32))
            .ok_or_else(|| Problem::new("操作 ID 无效，请刷新页面。"))?
            .to_owned();
        let digest = operation_digest(action, body);
        if let Some(receipt) = self.receipts.get(&operation).cloned() {
            if receipt.get("digest").and_then(Value::as_str) != Some(digest.as_str()) {
                return Err(Problem::code("操作 ID 已被其他请求使用。", "id_conflict"));
            }
            if receipt.get("state").and_then(Value::as_str) == Some("complete") {
                return Ok(receipt.get("result").cloned().unwrap_or(Value::Null));
            }
            let error = receipt.get("error").and_then(Value::as_object);
            return Err(Problem {
                message: error
                    .and_then(|value| value.get("message"))
                    .and_then(Value::as_str)
                    .unwrap_or("上次操作结果未确认，请刷新状态后再决定下一步。")
                    .to_owned(),
                code: error
                    .and_then(|value| value.get("code"))
                    .and_then(Value::as_str)
                    .unwrap_or("outcome_unknown")
                    .to_owned(),
                uncertain: error
                    .and_then(|value| value.get("uncertain"))
                    .and_then(Value::as_bool)
                    .unwrap_or(true),
                details: None,
            });
        }
        if !matches!(
            action,
            "apply" | "start" | "stop" | "restart_forward" | "reconnect" | "restart_agent"
        ) {
            return Err(Problem::new("不支持此操作。"));
        }
        if self.receipts.len() >= MAX_RECEIPTS {
            let oldest = self
                .receipts
                .iter()
                .find(|(_, receipt)| {
                    receipt.get("state").and_then(Value::as_str) != Some("pending")
                })
                .map(|(id, _)| id.clone());
            if let Some(oldest) = oldest {
                self.receipts.remove(&oldest);
            } else {
                return Err(Problem::new("未确认的操作过多，请先核对操作记录。"));
            }
        }
        let mut subject = body_object
            .get("rule")
            .filter(|value| truthy(value))
            .or_else(|| body_object.get("peer").filter(|value| truthy(value)))
            .map(python_text)
            .unwrap_or_else(|| format!("{} Agent", self.local_name));
        if action == "apply" {
            let plan_id = body_object
                .get("plan")
                .and_then(Value::as_str)
                .unwrap_or("");
            if let Some(rule) = self.plans.get(plan_id).map(|plan| &plan.rule) {
                subject = format!(
                    "{} · {}",
                    rule.get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("配置草稿"),
                    rule.get("listen").and_then(Value::as_str).unwrap_or("")
                )
                .trim_matches(|character| character == ' ' || character == '·')
                .to_owned();
            }
        }
        self.receipts.insert(
            operation.clone(),
            json!({
                "action": action,
                "at": stamp(),
                "digest": digest,
                "state": "pending",
                "subject": subject,
            }),
        );
        write_private(&self.receipts_path, &Value::Object(self.receipts.clone()))?;
        let result = if action == "apply" {
            let plan_id = body_object
                .get("plan")
                .and_then(Value::as_str)
                .unwrap_or("");
            self.apply(plan_id, &operation).await
        } else {
            self.action(action, body, &operation).await
        };
        match result {
            Ok(result) => {
                if let Some(receipt) = self
                    .receipts
                    .get_mut(&operation)
                    .and_then(Value::as_object_mut)
                {
                    receipt.insert("state".into(), json!("complete"));
                    receipt.insert("result".into(), result.clone());
                }
                if write_private(&self.receipts_path, &Value::Object(self.receipts.clone()))
                    .is_err()
                {
                    return Err(Problem::unknown(
                        "管理动作已执行，但操作回执未能持久化。请刷新状态核对，勿直接重复提交。",
                    ));
                }
                Ok(result)
            }
            Err(error) => {
                if let Some(receipt) = self
                    .receipts
                    .get_mut(&operation)
                    .and_then(Value::as_object_mut)
                {
                    receipt.insert(
                        "state".into(),
                        json!(if error.uncertain { "unknown" } else { "failed" }),
                    );
                    receipt.insert(
                        "error".into(),
                        json!({
                            "message": error.message,
                            "code": error.code,
                            "uncertain": error.uncertain,
                        }),
                    );
                }
                if write_private(&self.receipts_path, &Value::Object(self.receipts.clone()))
                    .is_err()
                {
                    return Err(Problem::unknown(
                        "管理动作结果和操作回执均未确认。请刷新状态核对，勿直接重复提交。",
                    ));
                }
                Err(error)
            }
        }
    }

    pub async fn diagnose(&mut self, rule_id: &str) -> Result<Value> {
        let config = self.local_config().await?;
        let settings = required_object(&config, "settings", "config.settings")?;
        let rule = settings
            .get("forwards")
            .and_then(Value::as_array)
            .and_then(|forwards| {
                forwards
                    .iter()
                    .find(|rule| rule.get("id").and_then(Value::as_str) == Some(rule_id))
            })
            .cloned()
            .ok_or_else(|| Problem::new("映射不存在，请刷新状态。"))?;
        let status = self
            .backend_call(json!({"method":"status"}), false, None)
            .await?;
        let runtime = status
            .pointer("/forwards/rules")
            .and_then(Value::as_array)
            .and_then(|rules| {
                rules
                    .iter()
                    .find(|rule| rule.get("id").and_then(Value::as_str) == Some(rule_id))
            })
            .cloned()
            .unwrap_or(json!({}));
        let managed = self.managed_peer(&Value::Object(settings.clone()), false)?;
        let sid = settings
            .get("service_ids")
            .and_then(Value::as_object)
            .and_then(|ids| ids.get(rule_id))
            .and_then(Value::as_str)
            .ok_or_else(|| Problem::new("映射缺少服务 ID。"))?
            .to_owned();
        let mut result = json!({
            "checkedAt": stamp(),
            "rule": rule_id,
            "listening": runtime.get("listening").and_then(Value::as_bool).unwrap_or(false),
            "peerReady": runtime.get("peer_ready").and_then(Value::as_bool).unwrap_or(false),
            "lastError": runtime.get("last_error").cloned().unwrap_or(Value::Null),
            "target": null,
            "tunnel": null,
            "http": null,
        });
        if rule.get("peer_node_id").and_then(Value::as_str) == managed.as_deref() {
            match self.bridge.target_probe(&sid).await {
                Ok(target) => result["target"] = target,
                Err(error) => result["targetError"] = json!(error.message),
            }
        }
        if result.get("listening").and_then(Value::as_bool) != Some(true)
            || result.get("peerReady").and_then(Value::as_bool) != Some(true)
        {
            return Ok(result);
        }
        let listen = address(
            rule.get("listen")
                .and_then(Value::as_str)
                .ok_or_else(|| Problem::new("映射监听地址无效。"))?,
            true,
        )?;
        let baseline = self.baseline()?;
        let navigation = self.navigation(&config, &baseline)?;
        let service = navigation
            .get("services")
            .and_then(Value::as_array)
            .and_then(|services| {
                services
                    .iter()
                    .find(|service| service.get("id").and_then(Value::as_str) == Some(rule_id))
            })
            .cloned()
            .ok_or_else(|| Problem::new("导航中缺少映射服务。"))?;
        let health_path = service
            .get("healthPath")
            .and_then(Value::as_str)
            .unwrap_or("");
        let path = if health_path.is_empty() {
            url_path(service.get("url").and_then(Value::as_str).unwrap_or(""))
        } else {
            health_path.to_owned()
        };
        let protocol = service
            .get("protocol")
            .and_then(Value::as_str)
            .unwrap_or("tcp");
        if matches!(protocol, "http" | "https") {
            match http_probe(listen, &path, protocol == "https").await {
                Ok(code) => {
                    result["http"] = json!({
                        "status": code,
                        "path": path,
                        "healthCheck": !health_path.is_empty(),
                        "healthy": if health_path.is_empty() { Value::Null } else { Value::Bool((200..300).contains(&code)) },
                        "tlsVerified": if protocol == "https" { json!(false) } else { Value::Null },
                    });
                    result["tunnel"] = json!(true);
                }
                Err(_) => {
                    result["tunnel"] = json!(false);
                    result["probeError"] =
                        json!("映射没有返回有效 HTTP 响应，可能是目标服务、协议或 P2P 通道异常。");
                }
            }
        } else {
            result["probeNote"] =
                json!("非 HTTP 服务：仅显示监听、P2P 和独立目标检测，未验证应用协议。");
        }
        Ok(result)
    }

    fn baseline(&self) -> Result<Value> {
        let html = fs::read_to_string(&self.page)
            .map_err(|error| Problem::new(format!("无法读取导航页：{error}")))?;
        let block = navigation_block(&html)?;
        serde_json::from_str(&block).map_err(Problem::from)
    }

    async fn local_config(&self) -> Result<Value> {
        self.backend_call(json!({"method":"config"}), false, None)
            .await
    }

    async fn backend_call(
        &self,
        command: Value,
        remote: bool,
        request_id: Option<&str>,
    ) -> Result<Value> {
        self.bridge.call(command, remote, request_id).await
    }

    fn managed_peer(&self, settings: &Value, required: bool) -> Result<Option<String>> {
        let Some(remote) = self.config.get("remote").and_then(Value::as_object) else {
            if required {
                return Err(Problem::code(
                    "未配置独立远端管理通道。",
                    "remote_not_configured",
                ));
            }
            return Ok(None);
        };
        let management_id = remote
            .get("management_service_id")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty());
        let service_ids = settings
            .get("service_ids")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let peers = settings
            .get("forwards")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|rule| {
                let rule_id = rule.get("id").and_then(Value::as_str);
                management_id.is_some()
                    && rule_id
                        .and_then(|id| service_ids.get(id))
                        .and_then(Value::as_str)
                        == management_id
            })
            .filter_map(|rule| rule.get("peer_node_id").and_then(Value::as_str))
            .map(str::to_owned)
            .collect::<BTreeSet<_>>();
        let pinned = remote
            .get("node_id")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_owned);
        if peers.len() > 1
            || pinned.as_ref().is_some_and(|pinned| {
                !peers.is_empty() && (peers.len() != 1 || !peers.contains(pinned))
            })
        {
            return Err(Problem::code(
                "管理映射与配置的远端节点身份不一致，已拒绝远端操作。",
                "identity_mismatch",
            ));
        }
        let result = pinned.or_else(|| peers.into_iter().next());
        if required && result.is_none() {
            return Err(Problem::code(
                "无法识别远端节点，请配置完整节点 ID 或保留管理服务映射。",
                "identity_mismatch",
            ));
        }
        Ok(result)
    }

    async fn remote_config(&mut self) -> Result<Value> {
        let result: Result<Value> = async {
            let local = self.local_config().await?;
            let expected = self.managed_peer(
                local
                    .get("settings")
                    .ok_or_else(|| Problem::new("本机配置缺少 settings。"))?,
                true,
            )?;
            let actual = self
                .backend_call(json!({"method":"status"}), true, None)
                .await?
                .pointer("/node/node_id")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| Problem::new("远端状态缺少节点 ID。"))?;
            if Some(actual) != expected {
                return Err(Problem::code(
                    "独立 SSH 连接的节点身份与配置不一致，已拒绝操作。",
                    "identity_mismatch",
                ));
            }
            self.backend_call(json!({"method":"config"}), true, None)
                .await
        }
        .await;
        match result {
            Ok(config) => {
                self.remote = Some(config.clone());
                self.remote_error = None;
                Ok(config)
            }
            Err(error) => {
                self.remote_error = Some(error.message.clone());
                Err(error)
            }
        }
    }

    fn navigation(&self, config: &Value, baseline: &Value) -> Result<Value> {
        let settings = required_object(config, "settings", "config.settings")?;
        let settings_value = Value::Object(settings.clone());
        let old = baseline
            .get("services")
            .and_then(Value::as_array)
            .ok_or_else(|| Problem::new("导航数据缺少 services。"))?;
        let managed = self.managed_peer(&settings_value, false).ok().flatten();
        let remote_services = self
            .remote
            .as_ref()
            .and_then(|value| value.pointer("/settings/services"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut services = Vec::new();
        for rule in settings
            .get("forwards")
            .and_then(Value::as_array)
            .ok_or_else(|| Problem::new("配置缺少 forwards。"))?
        {
            let rule_id = rule
                .get("id")
                .and_then(Value::as_str)
                .ok_or_else(|| Problem::new("转发规则缺少 ID。"))?;
            let service_id = settings
                .get("service_ids")
                .and_then(Value::as_object)
                .and_then(|ids| ids.get(rule_id))
                .and_then(Value::as_str)
                .unwrap_or("");
            let peer = rule
                .get("peer_node_id")
                .and_then(Value::as_str)
                .ok_or_else(|| Problem::new("转发规则缺少远端节点。"))?;
            let prior = old
                .iter()
                .find(|service| service.get("id").and_then(Value::as_str) == Some(rule_id))
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            let same = prior.get("peerNodeId").and_then(Value::as_str) == Some(peer)
                && prior.get("serviceId").and_then(Value::as_str) == Some(service_id);
            let mut service = if same { prior.clone() } else { Map::new() };
            let server = if managed.as_deref() == Some(peer) {
                remote_services
                    .iter()
                    .find(|server| server.get("id").and_then(Value::as_str) == Some(service_id))
            } else {
                None
            };
            let mut meta = self
                .metadata
                .get(rule_id)
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            if meta.get("peer").and_then(Value::as_str) != Some(peer)
                || meta.get("serviceId").and_then(Value::as_str) != Some(service_id)
            {
                meta.clear();
            }
            let protocol = meta
                .get("protocol")
                .and_then(Value::as_str)
                .or_else(|| prior.get("protocol").and_then(Value::as_str))
                .unwrap_or("tcp")
                .to_owned();
            let path = meta
                .get("path")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| {
                    url_path(prior.get("url").and_then(Value::as_str).unwrap_or(""))
                });
            let name = rule
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or(service_id);
            let title = meta
                .get("title")
                .and_then(Value::as_str)
                .filter(|value| !value.is_empty())
                .or_else(|| {
                    prior
                        .get("title")
                        .and_then(Value::as_str)
                        .filter(|value| !value.is_empty())
                })
                .unwrap_or(name);
            let target = server
                .and_then(|server| server.get("target"))
                .cloned()
                .or_else(|| prior.get("target").cloned())
                .unwrap_or_else(|| json!("未核实"));
            let enabled = rule
                .get("enabled")
                .and_then(Value::as_bool)
                .unwrap_or(false)
                && server.is_none_or(|server| {
                    server
                        .get("enabled")
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                });
            let listen = string_field(rule, "listen").unwrap_or("");
            service.insert("id".into(), json!(rule_id));
            service.insert("serviceId".into(), json!(service_id));
            service.insert("peerNodeId".into(), json!(peer));
            service.insert("name".into(), json!(name));
            service.insert("title".into(), json!(title));
            service.insert(
                "description".into(),
                prior
                    .get("description")
                    .cloned()
                    .unwrap_or_else(|| json!("在此管理端口映射或复制地址，使用对应客户端连接。")),
            );
            service.insert("listen".into(), json!(listen));
            service.insert("enabled".into(), json!(enabled));
            service.insert(
                "ruleEnabled".into(),
                json!(
                    rule.get("enabled")
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                ),
            );
            service.insert(
                "healthPath".into(),
                meta.get("healthPath").cloned().unwrap_or_else(|| json!("")),
            );
            service.insert(
                "autoStart".into(),
                json!(
                    rule.get("auto_start")
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                ),
            );
            service.insert("remote".into(), json!(true));
            service.insert("protocol".into(), json!(protocol));
            service.insert(
                "category".into(),
                meta.get("category")
                    .cloned()
                    .or_else(|| prior.get("category").cloned())
                    .unwrap_or_else(|| json!("其他")),
            );
            service.insert(
                "remoteLabel".into(),
                json!(format!(
                    "远端 {}",
                    self.peer_label(peer, &settings_value, baseline)
                )),
            );
            service.insert("target".into(), target.clone());
            service.insert("targetVerified".into(), json!(server.is_some()));
            service.insert(
                "targetCurrent".into(),
                json!(server.is_some() && self.remote_error.is_none()),
            );
            let url = if protocol == "tcp" {
                listen.to_owned()
            } else {
                let suffix = if matches!(protocol.as_str(), "http" | "https") {
                    path.as_str()
                } else {
                    ""
                };
                format!("{protocol}://{listen}{suffix}")
            };
            service.insert("url".into(), json!(url));
            if prior.get("target") != Some(&target) {
                service.remove("login");
                service.remove("loginUrl");
            } else if let Some(login_url) = prior.get("loginUrl").and_then(Value::as_str) {
                let without_fragment = login_url.split('#').next().unwrap_or("");
                let query = without_fragment
                    .find('?')
                    .map(|index| without_fragment[index..].to_owned())
                    .unwrap_or_default();
                service.insert(
                    "loginUrl".into(),
                    json!(format!("{protocol}://{listen}{path}{query}")),
                );
            }
            services.push(Value::Object(service));
        }
        services.extend(
            old.iter()
                .filter(|service| {
                    service.get("remote").and_then(Value::as_bool) != Some(true)
                        && service.get("category").and_then(Value::as_str) != Some("节点管理")
                        && !matches!(
                            service.get("id").and_then(Value::as_str),
                            Some("mac-admin" | "local-admin")
                        )
                })
                .cloned(),
        );
        services.push(json!({
            "id": required_string(&self.host, "localAdminId", "host.localAdminId")?,
            "name": format!("{} 节点管理", self.local_name),
            "title": format!("{} 节点管理", self.local_name),
            "description": "本机直接访问，管理此设备的服务与授权。",
            "protocol": "http",
            "category": "节点管理",
            "url": required_string(&self.host, "adminUrl", "host.adminUrl")?,
            "remote": false,
            "enabled": true,
            "autoStart": false,
        }));
        Ok(json!({
            "updatedAt": stamp(),
            "host": self.host,
            "services": services,
        }))
    }

    fn peer_label(&self, peer: &str, settings: &Value, baseline: &Value) -> String {
        if let Some(name) = self
            .config
            .get("peers")
            .and_then(Value::as_object)
            .and_then(|peers| peers.get(peer))
            .and_then(Value::as_object)
            .and_then(|peer| peer.get("name"))
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
        {
            return name.to_owned();
        }
        if self.managed_peer(settings, false).ok().flatten().as_deref() == Some(peer) {
            return self.remote_name.clone();
        }
        let label = baseline
            .get("services")
            .and_then(Value::as_array)
            .and_then(|services| {
                services
                    .iter()
                    .find(|service| service.get("peerNodeId").and_then(Value::as_str) == Some(peer))
            })
            .and_then(|service| service.get("remoteLabel"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .strip_prefix("远端 ")
            .unwrap_or("");
        if label.is_empty() || matches!(label, "远端设备" | "设备") {
            peer.chars().take(8).collect()
        } else {
            label.to_owned()
        }
    }

    fn protected_service(&self) -> Option<String> {
        self.config
            .get("remote")
            .and_then(Value::as_object)
            .and_then(|remote| remote.get("management_service_id"))
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    }

    fn check_port(&self, settings: &Value, rule: &Value, previous: Option<&Value>) -> Result<()> {
        let parsed_address = address(
            string_field(rule, "listen").ok_or_else(|| Problem::new("监听地址无效。"))?,
            true,
        )?;
        let rule_id = string_field(rule, "id").unwrap_or("");
        for current in settings
            .get("forwards")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if current.get("id").and_then(Value::as_str) == Some(rule_id) {
                continue;
            }
            if let Some(listen) = current.get("listen").and_then(Value::as_str)
                && address(listen, true)? == parsed_address
            {
                return Err(Problem::code("监听端口已被另一条映射使用。", "port_in_use"));
            }
        }
        if previous.and_then(|value| value.get("listen")) == rule.get("listen") {
            return Ok(());
        }
        TcpListener::bind(parsed_address).map_err(|_| {
            Problem::code(
                format!("{} 监听端口已被占用，请换一个端口。", self.local_name),
                "port_in_use",
            )
        })?;
        Ok(())
    }

    fn prune_plans(&mut self) {
        let now = Instant::now();
        self.plans.retain(|_, plan| plan.expires > now);
    }

    async fn apply(&mut self, token: &str, operation: &str) -> Result<Value> {
        let Some(plan) = self.plans.get(token).cloned() else {
            return Err(Problem::code(
                "预览已过期，请重新检查配置。",
                "preview_expired",
            ));
        };
        if plan.expires <= Instant::now() {
            return Err(Problem::code(
                "预览已过期，请重新检查配置。",
                "preview_expired",
            ));
        }
        let current = self.local_config().await?;
        let current_revision = required_u64(&current, "revision", "config.revision")?;
        if current_revision != required_u64(&plan.config, "revision", "plan.config.revision")? {
            return Err(Problem::code(
                format!("{} 配置已变更，请重新预览；草稿仍保留。", self.local_name),
                "revision_conflict",
            ));
        }
        self.check_port(
            current
                .get("settings")
                .ok_or_else(|| Problem::new("本机配置缺少 settings。"))?,
            &plan.rule,
            plan.previous.as_ref(),
        )?;
        let remote_changed = plan
            .remote_before
            .as_ref()
            .is_some_and(|before| before.get("settings") != plan.remote_after.as_ref());
        if plan.remote_before.is_some() {
            let now = self.remote_config().await?;
            if required_u64(&now, "revision", "remote.revision")?
                != required_u64(
                    plan.remote_before.as_ref().expect("remote checked"),
                    "revision",
                    "plan.remote.revision",
                )?
            {
                return Err(Problem::code(
                    format!("{} 配置已变更，请重新预览；草稿仍保留。", self.remote_name),
                    "revision_conflict",
                ));
            }
        }
        let remote_result = if remote_changed {
            let before = plan.remote_before.as_ref().expect("remote checked");
            Some(
                self.backend_call(
                    json!({
                        "method": "apply_config",
                        "expected_revision": before.get("revision").cloned().unwrap_or(Value::Null),
                        "settings": plan.remote_after.clone(),
                    }),
                    true,
                    Some(&format!("{operation}-remote")),
                )
                .await?,
            )
        } else {
            None
        };
        let local_settings = current
            .get("settings")
            .ok_or_else(|| Problem::new("本机配置缺少 settings。"))?;
        let local_result = if local_settings != &plan.settings {
            match self
                .backend_call(
                    json!({
                        "method": "apply_config",
                        "expected_revision": current_revision,
                        "settings": plan.settings.clone(),
                    }),
                    false,
                    Some(&format!("{operation}-local")),
                )
                .await
            {
                Ok(result) => result,
                Err(error) => {
                    if let Some(remote_result) = &remote_result {
                        if error.uncertain {
                            return Err(Problem {
                                message: format!(
                                    "{} 已保存，{} 结果尚未确认。请刷新两端配置后重新预览，未自动回滚。",
                                    self.remote_name, self.local_name
                                ),
                                code: "partial_unknown".into(),
                                uncertain: true,
                                details: Some(json!({
                                    "remoteRevision": remote_result.get("revision").cloned().unwrap_or(Value::Null)
                                })),
                            });
                        }
                        let before = plan.remote_before.as_ref().expect("remote checked");
                        if self
                            .backend_call(
                                json!({
                                    "method": "apply_config",
                                    "expected_revision": remote_result.get("revision").cloned().unwrap_or(Value::Null),
                                    "settings": before.get("settings").cloned().unwrap_or(Value::Null),
                                }),
                                true,
                                Some(&format!("{operation}-rollback")),
                            )
                            .await
                            .is_err()
                        {
                            return Err(Problem {
                                message: format!(
                                    "{} 保存失败，{} 回滚未确认。请刷新两端配置核对。",
                                    self.local_name, self.remote_name
                                ),
                                code: "partial_unknown".into(),
                                uncertain: true,
                                details: None,
                            });
                        }
                        return Err(Problem::code(
                            format!(
                                "{} 保存失败，{} 配置已恢复。原因：{}",
                                self.local_name, self.remote_name, error.message
                            ),
                            error.code,
                        ));
                    }
                    return Err(error);
                }
            }
        } else {
            json!({"revision":current_revision})
        };
        let rule_id = string_field(&plan.rule, "id")
            .ok_or_else(|| Problem::new("映射缺少规则 ID。"))?
            .to_owned();
        match &plan.meta {
            Some(meta) => {
                self.metadata.insert(rule_id.clone(), meta.clone());
            }
            None => {
                self.metadata.remove(&rule_id);
            }
        }
        let write_result: Result<()> = async {
            write_private(&self.meta_path, &Value::Object(self.metadata.clone()))?;
            if let Some(remote_result) = &remote_result {
                self.remote = Some(json!({
                    "revision": remote_result.get("revision").cloned().unwrap_or(Value::Null),
                    "settings": plan.remote_after.clone(),
                }));
            }
            self.persist_page(&json!({
                "revision": local_result.get("revision").cloned().unwrap_or(Value::Null),
                "settings": plan.settings.clone(),
            }))
        }
        .await;
        if write_result.is_err() {
            return Err(Problem::code(
                "Agent 配置已保存，导航文件更新失败。请刷新状态确认映射，勿重复提交。",
                "navigation_write_failed",
            )
            .with_uncertain());
        }
        self.plans.remove(token);
        let mut warnings = Vec::new();
        for result in [remote_result.as_ref(), Some(&local_result)]
            .into_iter()
            .flatten()
        {
            if let Some(warning) = result.get("durability_warning") {
                warnings.push(warning.clone());
            }
        }
        Ok(json!({
            "message": if plan.meta.is_none() {
                "此映射已移除，远端服务保留。"
            } else {
                "配置已保存，导航已更新。"
            },
            "ruleId": rule_id,
            "revision": local_result.get("revision").cloned().unwrap_or(Value::Null),
            "warnings": warnings,
        }))
    }

    fn persist_page(&self, config: &Value) -> Result<()> {
        let baseline = self.baseline()?;
        let navigation = self.navigation(config, &baseline)?;
        let mut encoded = serde_json::to_string_pretty(&navigation)?;
        encoded = encoded
            .replace('<', "\\u003c")
            .replace('>', "\\u003e")
            .replace('&', "\\u0026");
        let html = fs::read_to_string(&self.page)
            .map_err(|error| Problem::new(format!("无法读取导航页：{error}")))?;
        let opening = unique_index(&html, DATA_OPEN, "导航数据块")?;
        let value_start = opening + DATA_OPEN.len();
        let relative_close = html[value_start..]
            .find(DATA_CLOSE)
            .ok_or_else(|| Problem::new("导航数据块异常。"))?;
        let value_end = value_start + relative_close;
        let close_end = value_end + DATA_CLOSE.len();
        let replaced = format!(
            "{}{}\n{}\n{}{}",
            &html[..opening],
            DATA_OPEN,
            encoded,
            DATA_CLOSE,
            &html[close_end..]
        );
        write_private_bytes(&self.page, replaced.as_bytes())
    }

    async fn action(&mut self, action: &str, body: &Value, operation: &str) -> Result<Value> {
        let body = body
            .as_object()
            .ok_or_else(|| Problem::new("请求字段无效，请重新读取配置。"))?;
        if action == "restart_agent" {
            if body.get("confirm").and_then(Value::as_str) != Some("restart_all") {
                return Err(Problem::new(format!(
                    "请确认整台 {} Agent 重启会影响全部转发。",
                    self.local_name
                )));
            }
            return self.bridge.restart().await;
        }
        let config = self.local_config().await?;
        let settings = required_object(&config, "settings", "config.settings")?;
        if action == "reconnect" {
            let peer = body
                .get("peer")
                .and_then(Value::as_str)
                .ok_or_else(|| Problem::new("请选择要重新连接的设备。"))?;
            let known = settings
                .get("forwards")
                .and_then(Value::as_array)
                .is_some_and(|forwards| {
                    forwards
                        .iter()
                        .any(|rule| rule.get("peer_node_id").and_then(Value::as_str) == Some(peer))
                });
            if !known || body.get("confirm").and_then(Value::as_str) != Some("reconnect_peer") {
                return Err(Problem::new("请确认要重新连接的设备及影响范围。"));
            }
            self.backend_call(
                json!({"method":"disconnect","peer":peer}),
                false,
                Some(&format!("{operation}-disconnect")),
            )
            .await?;
            self.backend_call(
                json!({"method":"connect","peer":peer,"password":null}),
                false,
                Some(&format!("{operation}-connect")),
            )
            .await?;
            return Ok(json!({"message":"已请求重新连接设备；请刷新状态确认认证结果。"}));
        }
        let rule_id = body
            .get("rule")
            .and_then(Value::as_str)
            .ok_or_else(|| Problem::new("映射不存在，请刷新状态。"))?;
        let rule = settings
            .get("forwards")
            .and_then(Value::as_array)
            .and_then(|forwards| {
                forwards
                    .iter()
                    .find(|rule| rule.get("id").and_then(Value::as_str) == Some(rule_id))
            })
            .cloned()
            .ok_or_else(|| Problem::new("映射不存在，请刷新状态。"))?;
        if action == "restart_forward"
            && body.get("confirm").and_then(Value::as_str) != Some("restart_rule")
        {
            return Err(Problem::new("请确认重启此条转发。"));
        }
        if action != "stop" && rule.get("enabled").and_then(Value::as_bool) != Some(true) {
            return Err(Problem::new("规则已禁用，请先在配置中启用。"));
        }
        if matches!(action, "stop" | "restart_forward") {
            self.backend_call(
                json!({"method":"stop_forward","rule":rule_id}),
                false,
                Some(&format!("{operation}-stop")),
            )
            .await?;
        }
        if matches!(action, "start" | "restart_forward") {
            self.backend_call(
                json!({"method":"start_forward","rule":rule_id}),
                false,
                Some(&format!("{operation}-start")),
            )
            .await?;
        }
        let message = match action {
            "start" => "本条转发已启动。",
            "stop" => "本条转发已停止；保存的启用/自动启动配置未改变。",
            "restart_forward" => "本条转发已重新启动。",
            _ => return Err(Problem::new("不支持此操作。")),
        };
        Ok(json!({"message":message}))
    }

    fn history(&self) -> Vec<Value> {
        let mut entries = self
            .receipts
            .iter()
            .map(|(id, receipt)| (id, receipt))
            .collect::<Vec<_>>();
        entries.sort_by(|(left_id, left), (right_id, right)| {
            let left_at = left.get("at").and_then(Value::as_str).unwrap_or("");
            let right_at = right.get("at").and_then(Value::as_str).unwrap_or("");
            match (
                epoch_seconds_from_iso_stamp(left_at),
                epoch_seconds_from_iso_stamp(right_at),
            ) {
                (Some(left_epoch), Some(right_epoch)) => left_epoch.cmp(&right_epoch),
                _ => left_at.cmp(right_at),
            }
            .then_with(|| left_id.cmp(right_id))
        });
        entries
            .into_iter()
            .rev()
            .take(15)
            .map(|(id, receipt)| {
                let message = receipt
                    .pointer("/result/message")
                    .and_then(Value::as_str)
                    .or_else(|| receipt.pointer("/error/message").and_then(Value::as_str))
                    .unwrap_or("执行结果未确认，请刷新状态。");
                json!({
                    "id": id,
                    "action": receipt.get("action").cloned().unwrap_or(Value::Null),
                    "at": receipt.get("at").cloned().unwrap_or(Value::Null),
                    "state": receipt.get("state").cloned().unwrap_or(Value::Null),
                    "subject": receipt.get("subject").cloned().unwrap_or_else(|| json!("")),
                    "message": message,
                })
            })
            .collect()
    }
}

trait ProblemExt {
    fn with_uncertain(self) -> Self;
}
impl ProblemExt for Problem {
    fn with_uncertain(mut self) -> Self {
        self.uncertain = true;
        self
    }
}

fn config_path(config: &Value, field: &str) -> Result<PathBuf> {
    let value = config
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| Problem::new(format!("配置缺少 {field} 路径。")))?;
    let path = PathBuf::from(value);
    if !path.is_absolute() || value.contains('\0') {
        return Err(Problem::new(format!("{field} 必须是绝对路径。")));
    }
    Ok(path)
}

fn read_map(path: &Path, label: &str) -> Result<Map<String, Value>> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice::<Value>(&bytes)
            .map_err(Problem::from)?
            .as_object()
            .cloned()
            .ok_or_else(|| Problem::new(format!("{label} 必须是 JSON 对象。"))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Map::new()),
        Err(error) => Err(Problem::new(format!("无法读取 {label}：{error}"))),
    }
}

fn required_object<'a>(
    value: &'a Value,
    field: &str,
    label: &str,
) -> Result<&'a Map<String, Value>> {
    value
        .get(field)
        .and_then(Value::as_object)
        .ok_or_else(|| Problem::new(format!("{label} 必须是对象。")))
}

fn required_object_value<'a>(value: &'a Value, label: &str) -> Result<&'a Map<String, Value>> {
    value
        .as_object()
        .ok_or_else(|| Problem::new(format!("{label} 必须是对象。")))
}

fn required_string<'a>(value: &'a Value, field: &str, label: &str) -> Result<&'a str> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| Problem::new(format!("{label} 必须是字符串。")))
}

fn nested_string<'a>(value: &'a Value, path: &[&str], label: &str) -> Result<&'a str> {
    let mut current = value;
    for field in path {
        current = current
            .get(*field)
            .ok_or_else(|| Problem::new(format!("缺少 {label}。")))?;
    }
    current
        .as_str()
        .ok_or_else(|| Problem::new(format!("{label} 必须是字符串。")))
}

fn required_u64(value: &Value, field: &str, label: &str) -> Result<u64> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| Problem::new(format!("{label} 必须是非负整数。")))
}

fn string_field<'a>(value: &'a Value, field: &str) -> Option<&'a str> {
    value.get(field).and_then(Value::as_str)
}

fn optional_string(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn form_text(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(value)) => value.clone(),
        Some(value) => python_text(value),
    }
}

fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(value) => value.as_f64().is_some_and(|value| value != 0.0),
        Value::String(value) => !value.is_empty(),
        Value::Array(value) => !value.is_empty(),
        Value::Object(value) => !value.is_empty(),
    }
}

fn known_peers(settings: &Value) -> Result<BTreeSet<String>> {
    let mut peers = settings
        .get("forwards")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|rule| rule.get("peer_node_id").and_then(Value::as_str))
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    peers.extend(
        settings
            .get("trusted_devices")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|device| device.get("node_id").and_then(Value::as_str))
            .map(str::to_owned),
    );
    Ok(peers)
}

fn navigation_block(html: &str) -> Result<String> {
    let opening = unique_index(html, DATA_OPEN, "导航数据块")?;
    let start = opening + DATA_OPEN.len();
    let end = start
        + html[start..]
            .find(DATA_CLOSE)
            .ok_or_else(|| Problem::new("导航数据缺失，请重新同步导航页。"))?;
    Ok(html[start..end].to_owned())
}

fn unique_index(html: &str, needle: &str, label: &str) -> Result<usize> {
    let mut positions = html.match_indices(needle);
    let position = positions
        .next()
        .map(|(position, _)| position)
        .ok_or_else(|| Problem::new(format!("{label}缺失，请重新同步导航页。")))?;
    if positions.next().is_some() {
        return Err(Problem::new(format!("{label}必须恰好出现一次。")));
    }
    Ok(position)
}

fn epoch_seconds_from_iso_stamp(value: &str) -> Option<i64> {
    let bytes = value.as_bytes();
    if bytes.len() < 20
        || bytes.get(4) != Some(&b'-')
        || bytes.get(7) != Some(&b'-')
        || !matches!(bytes.get(10), Some(b'T' | b't'))
        || bytes.get(13) != Some(&b':')
        || bytes.get(16) != Some(&b':')
    {
        return None;
    }
    let number = |start: usize, end: usize| {
        let digits = value.get(start..end)?;
        if !digits.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        digits.parse::<i64>().ok()
    };
    let year = number(0, 4)?;
    let month = number(5, 7)?;
    let day = number(8, 10)?;
    let hour = number(11, 13)?;
    let minute = number(14, 16)?;
    let second = number(17, 19)?;
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour > 23
        || minute > 59
        || second > 60
    {
        return None;
    }
    let mut cursor = 19;
    if bytes.get(cursor) == Some(&b'.') {
        cursor += 1;
        let fraction_start = cursor;
        while bytes.get(cursor).is_some_and(u8::is_ascii_digit) {
            cursor += 1;
        }
        if cursor == fraction_start {
            return None;
        }
    }
    let offset = match bytes.get(cursor) {
        Some(b'Z' | b'z') if cursor + 1 == bytes.len() => 0,
        Some(sign @ (b'+' | b'-')) => {
            cursor += 1;
            let offset_hour = number(cursor, cursor + 2)?;
            let offset_minute = if bytes.get(cursor + 2) == Some(&b':') {
                if cursor + 5 != bytes.len() {
                    return None;
                }
                number(cursor + 3, cursor + 5)?
            } else {
                if cursor + 4 != bytes.len() {
                    return None;
                }
                number(cursor + 2, cursor + 4)?
            };
            if offset_hour > 23 || offset_minute > 59 {
                return None;
            }
            let magnitude = offset_hour * 3600 + offset_minute * 60;
            if *sign == b'+' { magnitude } else { -magnitude }
        }
        _ => return None,
    };
    let adjusted_year = year - if month <= 2 { 1 } else { 0 };
    let era = adjusted_year.div_euclid(400);
    let year_of_era = adjusted_year - era * 400;
    let month_prime = month + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * month_prime + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    Some(days * 86_400 + hour * 3600 + minute * 60 + second - offset)
}

fn url_path(url: &str) -> String {
    let remainder = url
        .find("://")
        .map(|index| &url[index + 3..])
        .unwrap_or(url);
    let path = remainder
        .find('/')
        .map(|index| &remainder[index..])
        .unwrap_or("/");
    let end = path.find(['?', '#']).unwrap_or(path.len());
    if end == 0 {
        "/".into()
    } else {
        path[..end].to_owned()
    }
}

fn is_lower_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn python_text(value: &Value) -> String {
    match value {
        Value::String(value) => value.clone(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Null => "None".into(),
        _ => value.to_string(),
    }
}

fn python_round_1(value: f64) -> f64 {
    let scaled = value * 10.0;
    let floor = scaled.floor();
    let fraction = scaled - floor;
    let rounded = if (fraction - 0.5).abs() < f64::EPSILON {
        if (floor as i64) % 2 == 0 {
            floor
        } else {
            floor + 1.0
        }
    } else {
        scaled.round()
    };
    rounded / 10.0
}

fn operation_digest(action: &str, body: &Value) -> String {
    let bytes = python_json(&json!([action, body]));
    hex::encode(Sha256::digest(bytes.as_bytes()))
}

fn python_json(value: &Value) -> String {
    match value {
        Value::Null => "null".into(),
        Value::Bool(true) => "true".into(),
        Value::Bool(false) => "false".into(),
        Value::Number(number) => number.to_string(),
        Value::String(value) => python_json_string(value),
        Value::Array(values) => format!(
            "[{}]",
            values
                .iter()
                .map(python_json)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Value::Object(values) => format!(
            "{{{}}}",
            values
                .iter()
                .map(|(key, value)| format!("{}: {}", python_json_string(key), python_json(value)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

fn python_json_string(value: &str) -> String {
    let mut output = String::from("\"");
    for character in value.chars() {
        match character {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\u{08}' => output.push_str("\\b"),
            '\u{0c}' => output.push_str("\\f"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            character if (' '..='~').contains(&character) => output.push(character),
            character if (character as u32) <= 0xffff => {
                output.push_str(&format!("\\u{:04x}", character as u32));
            }
            character => {
                let code = character as u32 - 0x10000;
                let high = 0xd800 + (code >> 10);
                let low = 0xdc00 + (code & 0x3ff);
                output.push_str(&format!("\\u{high:04x}\\u{low:04x}"));
            }
        }
    }
    output.push('"');
    output
}

#[derive(Debug)]
struct InsecureVerifier {
    provider: Arc<quinn::rustls::crypto::CryptoProvider>,
}

impl quinn::rustls::client::danger::ServerCertVerifier for InsecureVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &quinn::rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[quinn::rustls::pki_types::CertificateDer<'_>],
        _server_name: &quinn::rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: quinn::rustls::pki_types::UnixTime,
    ) -> std::result::Result<quinn::rustls::client::danger::ServerCertVerified, quinn::rustls::Error>
    {
        Ok(quinn::rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &quinn::rustls::pki_types::CertificateDer<'_>,
        dss: &quinn::rustls::DigitallySignedStruct,
    ) -> std::result::Result<
        quinn::rustls::client::danger::HandshakeSignatureValid,
        quinn::rustls::Error,
    > {
        quinn::rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &quinn::rustls::pki_types::CertificateDer<'_>,
        dss: &quinn::rustls::DigitallySignedStruct,
    ) -> std::result::Result<
        quinn::rustls::client::danger::HandshakeSignatureValid,
        quinn::rustls::Error,
    > {
        quinn::rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<quinn::rustls::SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

async fn http_probe(listen: SocketAddr, path: &str, https: bool) -> std::io::Result<u16> {
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\nUser-Agent: Veytrane-Navigation/1\r\n\r\n",
        listen
    );
    tokio::time::timeout(Duration::from_secs(6), async {
        let mut tcp = tokio::net::TcpStream::connect(listen).await?;
        if https {
            let provider = Arc::new(quinn::rustls::crypto::ring::default_provider());
            let config = quinn::rustls::ClientConfig::builder_with_provider(provider.clone())
                .with_safe_default_protocol_versions()
                .map_err(std::io::Error::other)?
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(InsecureVerifier { provider }))
                .with_no_client_auth();
            let connector = tokio_rustls::TlsConnector::from(Arc::new(config));
            let server_name =
                quinn::rustls::pki_types::ServerName::try_from(listen.ip().to_string())
                    .map_err(std::io::Error::other)?;
            let mut tls = connector
                .connect(server_name, tcp)
                .await
                .map_err(std::io::Error::other)?;
            tls.write_all(request.as_bytes()).await?;
            read_http_status(&mut tls).await
        } else {
            tcp.write_all(request.as_bytes()).await?;
            read_http_status(&mut tcp).await
        }
    })
    .await
    .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "HTTP probe timed out"))?
}

async fn read_http_status(
    stream: &mut (impl tokio::io::AsyncRead + Unpin),
) -> std::io::Result<u16> {
    let mut response = Vec::with_capacity(4096);
    let mut chunk = [0_u8; 2048];
    while response.len() < 16 * 1024 && !response.windows(4).any(|window| window == b"\r\n\r\n") {
        let count = stream.read(&mut chunk).await?;
        if count == 0 {
            break;
        }
        response.extend_from_slice(&chunk[..count]);
    }
    let first = response
        .split(|byte| *byte == b'\n')
        .next()
        .unwrap_or_default();
    std::str::from_utf8(first)
        .ok()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|value| value.parse::<u16>().ok())
        .ok_or_else(|| std::io::Error::other("invalid HTTP status line"))
}

#[cfg(test)]
#[path = "manager_tests.rs"]
mod tests;
