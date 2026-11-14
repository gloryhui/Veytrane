//! Owner-managed navigation configuration and local/remote Agent bridging.
//!
//! The local path always uses the running Agent's private IPC endpoint. Remote
//! management is an optional fixed SSH bootstrap used only by the existing
//! Python helper; this module never starts another local core or Python server.

use super::{Backend, Call, Problem, Result};
use crate::node::api::client::AgentClient;
use crate::node::api::protocol::{self as ipc, Command, Request};
use crate::node::local::paths::AgentPaths;
use serde_json::{Value, json};
use std::{
    io,
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{io::AsyncWriteExt, process::Command as ProcessCommand, sync::watch};

const CONFIG_SCHEMA: u64 = 1;
const PRIVATE_FILE_LIMIT: u64 = 1024 * 1024;
const MANAGED_REMOTE_COMMAND_TIMEOUT: Duration = Duration::from_secs(40);

#[derive(Clone)]
pub struct Bridge {
    config: Value,
    paths: AgentPaths,
    restart: watch::Sender<bool>,
}

impl Bridge {
    pub fn new(config: Value, paths: AgentPaths, restart: watch::Sender<bool>) -> Result<Self> {
        validate_config(&config)?;
        verify_current_agent(&config, &paths)?;
        Ok(Self {
            config,
            paths,
            restart,
        })
    }

    async fn call_local(&self, command: Value, request_id: Option<&str>) -> Result<Value> {
        let command: Command = serde_json::from_value(command)
            .map_err(|error| Problem::new(format!("导航命令格式无效：{error}")))?;
        let mut request = Request::new(command);
        if let Some(request_id) = request_id {
            request.request_id = request_id.to_owned();
        }
        request
            .validate()
            .map_err(|error| Problem::new(error.to_string()))?;
        let response = AgentClient::new(self.paths.clone())
            .request(request)
            .await
            .map_err(agent_unavailable)?;
        response_result(response)
    }

    async fn call_remote(&self, request: Value) -> Result<Value> {
        let script = remote_script(&self.config, RemoteOperation::Request)?;
        let argv = ssh_argv(&self.config, &script)?;
        let input = serde_json::to_string(&request)?;
        let output = run_process(&argv, Some(input.as_bytes())).await?;
        remote_result(&output)
    }

    async fn probe_remote(&self, service: &str) -> Result<Value> {
        let script = remote_script(&self.config, RemoteOperation::Probe)?;
        let argv = ssh_argv(&self.config, &script)?;
        let input = serde_json::to_string(service)?;
        let output = run_process(&argv, Some(input.as_bytes())).await?;
        remote_result(&output)
    }
}

impl Backend for Bridge {
    fn call<'a>(&'a self, command: Value, remote: bool, request_id: Option<&'a str>) -> Call<'a> {
        Box::pin(async move {
            if remote {
                let mut request = Request::new(
                    serde_json::from_value(command.clone())
                        .map_err(|error| Problem::new(format!("导航命令格式无效：{error}")))?,
                );
                if let Some(request_id) = request_id {
                    request.request_id = request_id.to_owned();
                }
                request
                    .validate()
                    .map_err(|error| Problem::new(error.to_string()))?;
                self.call_remote(serde_json::to_value(request)?).await
            } else {
                self.call_local(command, request_id).await
            }
        })
    }

    fn target_probe<'a>(&'a self, service: &'a str) -> Call<'a> {
        Box::pin(self.probe_remote(service))
    }

    fn restart<'a>(&'a self) -> Call<'a> {
        Box::pin(async move {
            verify_current_agent(&self.config, &self.paths)?;
            if self.restart.receiver_count() == 0 {
                return Err(Problem::code(
                    "宿主未连接内建重启通道。",
                    "restart_unavailable",
                ));
            }
            let local_name = config_string(&self.config, "local_name")?.to_owned();
            let restart = self.restart.clone();
            tokio::spawn(async move {
                // The HTTP layer persists its accepted receipt before this wakes.
                tokio::time::sleep(Duration::from_secs(1)).await;
                restart.send_modify(|value| *value = !*value);
            });
            Ok(json!({
                "accepted": true,
                "message": format!(
                    "已请求重启 {local_name} Agent，请稍后刷新状态。"
                ),
            }))
        })
    }
}

/// Loads an owner-managed navigation configuration.
///
/// If `path` does not exist, the returned defaults are derived from its parent
/// directory. Nothing is written here, including the missing config file.
pub fn load_config(path: &Path) -> Result<Value> {
    let config_path = absolute_path(path)?;
    let parent = config_path
        .parent()
        .ok_or_else(|| Problem::new("导航配置路径缺少父目录。"))?;
    let platform = current_platform()?;
    let defaults = default_config_for(platform, parent)?;

    let source = match std::fs::symlink_metadata(&config_path) {
        Ok(metadata) => {
            if !crate::storage::atomic::regular_file(&metadata)
                || metadata.len() > PRIVATE_FILE_LIMIT
            {
                return Err(Problem::new("导航配置必须是最大 1 MiB 的普通文件。"));
            }
            let bytes = std::fs::read(&config_path)?;
            let value: Value = serde_json::from_slice(&bytes)
                .map_err(|error| Problem::new(format!("导航配置 JSON 无效：{error}")))?;
            if !value.is_object() {
                return Err(Problem::new("导航配置必须是 JSON 对象。"));
            }
            Some(value)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };

    let mut config = normalize_config(defaults, source.as_ref(), &config_path)?;
    config["config_file"] = json!(path_text(&config_path)?);
    validate_config(&config)?;
    Ok(config)
}

pub fn host_info(config: &Value) -> Result<Value> {
    validate_config(config)?;
    let platform = config_string(config, "platform")?;
    let remote_name = config
        .get("remote")
        .and_then(Value::as_object)
        .and_then(|remote| remote.get("name"))
        .cloned()
        .unwrap_or(Value::Null);
    let mut host = json!({
        "platform": platform,
        "name": config_string(config, "local_name")?,
        "localAdminId": if platform == "macos" { "mac-admin" } else { "local-admin" },
        "adminUrl": config_string(config, "agent_web")?,
        "managedRemoteName": remote_name,
        "agentRestartSupported": true,
        "ui": {"restartSupported": true},
    });
    if platform == "macos"
        && let Some(token_path) = installed_token_path(config)
    {
        host["tokenCommand"] = json!(format!(
            "pbcopy < {}",
            shell_quote(&path_text(&token_path)?)
        ));
    }
    Ok(host)
}

fn default_config_for(platform: &str, navigation_root: &Path) -> Result<Value> {
    let home = dirs::home_dir().ok_or_else(|| Problem::new("无法确定当前用户主目录。"))?;
    let data_root = dirs::data_local_dir()
        .ok_or_else(|| Problem::new("无法确定当前用户数据目录。"))?
        .join("p2p-file");
    let installed = match platform {
        "macos" | "windows" => data_root.join("bin").join("installed.json"),
        "linux" => home
            .join(".local")
            .join("lib")
            .join("p2p-file-node")
            .join("installed.json"),
        _ => return Err(platform_problem()),
    };
    let (shell, service, definition) = match platform {
        "macos" => (
            "launchd",
            Value::String("io.github.gloryhui.p2p-agent".into()),
            Value::String(
                path_text(
                    &home
                        .join("Library")
                        .join("LaunchAgents")
                        .join("io.github.gloryhui.p2p-agent.plist"),
                )?
                .to_owned(),
            ),
        ),
        "windows" => ("scheduled-task", Value::Null, Value::Null),
        "linux" => (
            "systemd",
            Value::String("p2p-file-agent.service".into()),
            Value::String(
                path_text(
                    &dirs::config_dir()
                        .ok_or_else(|| Problem::new("无法确定当前用户配置目录。"))?
                        .join("systemd")
                        .join("user")
                        .join("p2p-file-agent.service"),
                )?
                .to_owned(),
            ),
        ),
        _ => return Err(platform_problem()),
    };
    Ok(json!({
        "schema_version": CONFIG_SCHEMA,
        "platform": platform,
        "local_name": match platform {
            "macos" => "Mac",
            "windows" => "Windows",
            "linux" => "Linux",
            _ => return Err(platform_problem()),
        },
        "installed": path_text(&installed)?,
        "page": path_text(&navigation_root.join("service-navigation.html"))?,
        "state_dir": path_text(&navigation_root.join("control"))?,
        "navigation_root": path_text(navigation_root)?,
        "http": {"port": 3001, "public_port": 3001},
        "remote": if platform == "macos" {
            json!({
                "name": "Ubuntu",
                "node_id": null,
                "management_service_id": "ubuntu-admin",
                "ssh_host": "ubuntu",
                "python": "python3",
                "installed": "~/.local/lib/p2p-file-node/installed.json",
            })
        } else {
            Value::Null
        },
        "peers": {},
        "lifecycle": {
            "kind": shell,
            "service": service,
            "definition": definition,
        },
        "agent_web": "http://127.0.0.1:7789/",
    }))
}

fn normalize_config(
    mut defaults: Value,
    source: Option<&Value>,
    config_path: &Path,
) -> Result<Value> {
    let parent = config_path
        .parent()
        .ok_or_else(|| Problem::new("导航配置路径缺少父目录。"))?;
    if let Some(source) = source {
        let source = source
            .as_object()
            .ok_or_else(|| Problem::new("导航配置必须是 JSON 对象。"))?;
        if source
            .get("schema_version")
            .is_some_and(|value| value.as_u64() != Some(CONFIG_SCHEMA))
        {
            return Err(Problem::new("导航配置 schema_version 必须为 1。"));
        }
        let allowed = [
            "schema_version",
            "platform",
            "local_name",
            "installed",
            "page",
            "state_dir",
            "http",
            "remote",
            "peers",
            "lifecycle",
            "agent_web",
        ];
        let allowed: std::collections::HashSet<_> = allowed.into_iter().collect();
        let unknown: Vec<_> = source
            .keys()
            .filter(|key| !allowed.contains(key.as_str()))
            .cloned()
            .collect();
        if !unknown.is_empty() {
            return Err(Problem::new(format!(
                "未知导航配置项：{}",
                unknown.join(", ")
            )));
        }
        let object = defaults
            .as_object_mut()
            .ok_or_else(|| Problem::new("默认导航配置无效。"))?;
        for (key, value) in source {
            match key.as_str() {
                "http" | "lifecycle" => {
                    let overlay = value
                        .as_object()
                        .ok_or_else(|| Problem::new(format!("{key} 必须为配置对象。")))?;
                    let target = object
                        .get_mut(key)
                        .and_then(Value::as_object_mut)
                        .ok_or_else(|| Problem::new(format!("{key} 默认配置无效。")))?;
                    for (nested_key, nested_value) in overlay {
                        target.insert(nested_key.clone(), nested_value.clone());
                    }
                }
                "remote" if value.is_null() => {
                    object.insert(key.clone(), Value::Null);
                }
                "remote" => {
                    let overlay = value
                        .as_object()
                        .ok_or_else(|| Problem::new("remote 必须为配置对象或 null。"))?;
                    if !object.get("remote").is_some_and(Value::is_object) {
                        object.insert("remote".into(), default_remote());
                    }
                    let target = object
                        .get_mut("remote")
                        .and_then(Value::as_object_mut)
                        .ok_or_else(|| Problem::new("remote 默认配置无效。"))?;
                    for (nested_key, nested_value) in overlay {
                        target.insert(nested_key.clone(), nested_value.clone());
                    }
                }
                _ => {
                    object.insert(key.clone(), value.clone());
                }
            }
        }
    }

    if defaults["platform"] == "macos"
        && defaults["http"]["port"].as_u64() == Some(17790)
        && defaults["http"]["public_port"].as_u64() == Some(3001)
    {
        defaults["http"]["port"] = json!(3001);
        defaults["http"]["public_port"] = json!(3001);
    }

    for key in ["installed", "page", "state_dir"] {
        let raw = config_string(&defaults, key)?;
        let path = resolve_path(raw, parent)?;
        defaults[key] = json!(path_text(&path)?);
    }
    let lifecycle_definition = defaults
        .get("lifecycle")
        .and_then(Value::as_object)
        .and_then(|lifecycle| lifecycle.get("definition"))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    if let Some(definition) = lifecycle_definition {
        let path = resolve_path(&definition, parent)?;
        defaults["lifecycle"]["definition"] = json!(path_text(&path)?);
    }
    let navigation_root = Path::new(config_string(&defaults, "page")?)
        .parent()
        .ok_or_else(|| Problem::new("导航页面路径缺少父目录。"))?
        .to_path_buf();
    defaults["navigation_root"] = json!(path_text(&navigation_root)?);
    Ok(defaults)
}

fn validate_config(config: &Value) -> Result<()> {
    let object = config
        .as_object()
        .ok_or_else(|| Problem::new("导航配置必须是 JSON 对象。"))?;
    if object.get("schema_version").and_then(Value::as_u64) != Some(CONFIG_SCHEMA) {
        return Err(Problem::new("导航配置 schema_version 必须为 1。"));
    }
    let expected_platform = current_platform()?;
    let platform = config_string(config, "platform")?;
    if platform != expected_platform {
        return Err(Problem::new("导航配置平台与当前系统不匹配。"));
    }
    let local_name = config_string(config, "local_name")?.trim();
    if local_name.is_empty() || local_name.chars().count() > 64 {
        return Err(Problem::new("local_name 应为 1 到 64 字符的设备名称。"));
    }
    for key in [
        "installed",
        "page",
        "state_dir",
        "navigation_root",
        "agent_web",
    ] {
        if config_string(config, key)?.is_empty() {
            return Err(Problem::new(format!("{key} 不能为空。")));
        }
    }
    for key in ["installed", "page", "state_dir"] {
        if !Path::new(config_string(config, key)?).is_absolute() {
            return Err(Problem::new(format!("{key} 必须是绝对路径。")));
        }
    }
    let http = object
        .get("http")
        .and_then(Value::as_object)
        .ok_or_else(|| Problem::new("http 必须为配置对象。"))?;
    for key in ["port", "public_port"] {
        let port = http
            .get(key)
            .and_then(Value::as_u64)
            .ok_or_else(|| Problem::new("导航 HTTP 端口应为整数。"))?;
        if !(1024..=65535).contains(&port) {
            return Err(Problem::new("导航 HTTP 端口应在 1024 到 65535 之间。"));
        }
    }
    let lifecycle = object
        .get("lifecycle")
        .and_then(Value::as_object)
        .ok_or_else(|| Problem::new("lifecycle 必须为配置对象。"))?;
    let expected_kind = match platform {
        "macos" => "launchd",
        "windows" => "scheduled-task",
        "linux" => "systemd",
        _ => return Err(platform_problem()),
    };
    let kind = lifecycle
        .get("kind")
        .and_then(Value::as_str)
        .ok_or_else(|| Problem::new("lifecycle.kind 缺失。"))?;
    if kind != expected_kind && kind != "none" {
        return Err(Problem::new("节点启动方式与当前系统不匹配。"));
    }
    match lifecycle.get("service") {
        None | Some(Value::Null) => {}
        Some(Value::String(service)) if valid_service_name(service) => {}
        _ => return Err(Problem::new("节点系统服务名称无效。")),
    }
    match lifecycle.get("definition") {
        None | Some(Value::Null) => {}
        Some(Value::String(path)) if path.is_empty() => {}
        Some(Value::String(path)) if Path::new(path).is_absolute() => {}
        _ => return Err(Problem::new("lifecycle.definition 必须为绝对路径或 null。")),
    }
    let peers = object
        .get("peers")
        .and_then(Value::as_object)
        .ok_or_else(|| Problem::new("peers 必须为节点 ID 到设备信息的配置对象。"))?;
    for (peer, entry) in peers {
        if !is_node_id(peer) {
            return Err(Problem::new("peers 需要完整节点 ID。"));
        }
        let entry = entry
            .as_object()
            .ok_or_else(|| Problem::new("peers 设备信息必须为配置对象。"))?;
        if entry
            .get("name")
            .and_then(Value::as_str)
            .is_none_or(|name| name.trim().is_empty())
        {
            return Err(Problem::new("peers 需要非空 name。"));
        }
        match entry.get("platform") {
            None => {}
            Some(Value::String(platform))
                if matches!(platform.as_str(), "macos" | "windows" | "linux" | "unknown") => {}
            Some(_) => {
                return Err(Problem::new(
                    "peers.platform 应为 macos、windows、linux 或 unknown。",
                ));
            }
        }
        if let Some(targets) = entry.get("targets") {
            let targets = targets
                .as_object()
                .ok_or_else(|| Problem::new("peers.targets 必须为配置对象。"))?;
            for (service, target) in targets {
                if service.is_empty()
                    || target.as_str().is_none_or(|target| {
                        target.trim().is_empty() || target.chars().count() > 512
                    })
                {
                    return Err(Problem::new(
                        "peers.targets 应为服务 ID 到已确认目标地址的配置对象。",
                    ));
                }
            }
        }
    }
    if let Some(remote) = object.get("remote")
        && !remote.is_null()
    {
        let remote = remote
            .as_object()
            .ok_or_else(|| Problem::new("remote 必须为配置对象或 null。"))?;
        if remote
            .get("name")
            .and_then(Value::as_str)
            .is_none_or(|name| name.trim().is_empty())
        {
            return Err(Problem::new("remote 需要设备名称。"));
        }
        let node_id = match remote.get("node_id") {
            None | Some(Value::Null) => None,
            Some(Value::String(node_id)) if is_node_id(node_id) => Some(node_id.as_str()),
            Some(_) => {
                return Err(Problem::new("remote.node_id 必须为完整节点 ID。"));
            }
        };
        if remote
            .get("management_service_id")
            .is_some_and(|value| !value.is_null() && !value.is_string())
        {
            return Err(Problem::new(
                "remote.management_service_id 必须为字符串或 null。",
            ));
        }
        let service_id = remote
            .get("management_service_id")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty());
        if node_id.is_none() && service_id.is_none() {
            return Err(Problem::new(
                "独立 SSH 管理必须绑定节点 ID 或已有管理服务映射。",
            ));
        }
        let ssh_host = remote
            .get("ssh_host")
            .and_then(Value::as_str)
            .ok_or_else(|| Problem::new("remote.ssh_host 缺失。"))?;
        if ssh_host.is_empty()
            || ssh_host.starts_with('-')
            || ssh_host.chars().any(char::is_whitespace)
        {
            return Err(Problem::new("remote.ssh_host 必须为已有 SSH 主机或别名。"));
        }
        let python = remote
            .get("python")
            .and_then(Value::as_str)
            .ok_or_else(|| Problem::new("remote.python 缺失。"))?;
        if python.is_empty()
            || !python
                .bytes()
                .all(|value| value.is_ascii_alphanumeric() || b"_./-".contains(&value))
        {
            return Err(Problem::new("远端 Python 必须为命令名或不含空格的路径。"));
        }
        if remote
            .get("installed")
            .and_then(Value::as_str)
            .is_none_or(str::is_empty)
        {
            return Err(Problem::new("请配置远端 installed.json 路径。"));
        }
    }
    if !loopback_http_url(config_string(config, "agent_web")?) {
        return Err(Problem::new("agent_web 应为本机环回管理地址，不含凭据。"));
    }
    Ok(())
}

fn verify_current_agent(config: &Value, paths: &AgentPaths) -> Result<()> {
    validate_config(config)?;
    let installed = read_installed(config)?;
    let Some((bin_dir, data_dir)) = installed else {
        // A source checkout may run p2p-agent directly before installation.
        return Ok(());
    };
    if path_text(&bin_dir)?.is_empty() {
        return Err(Problem::code(
            "installed.json 的 bin_dir 无效，已拒绝导航管理。",
            "agent_host_mismatch",
        ));
    }
    let installed_data = std::fs::canonicalize(&data_dir).map_err(|error| {
        Problem::code(
            format!("无法核对 installed.json 的 data_dir：{error}"),
            "agent_host_mismatch",
        )
    })?;
    let current_data = std::fs::canonicalize(&paths.root).map_err(|error| {
        Problem::code(
            format!("无法核对当前 Agent data_dir：{error}"),
            "agent_host_mismatch",
        )
    })?;
    if !same_path(&installed_data, &current_data) {
        return Err(Problem::code(
            "installed.json 与当前运行的 Agent data_dir 不一致，已拒绝导航管理。",
            "agent_host_mismatch",
        ));
    }
    Ok(())
}

fn read_installed(config: &Value) -> Result<Option<(PathBuf, PathBuf)>> {
    let installed_path = Path::new(config_string(config, "installed")?);
    let metadata = match std::fs::symlink_metadata(installed_path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if !crate::storage::atomic::regular_file(&metadata) || metadata.len() > PRIVATE_FILE_LIMIT {
        return Err(Problem::code(
            "installed.json 必须是最大 1 MiB 的私有普通文件。",
            "agent_host_mismatch",
        ));
    }
    let bytes = std::fs::read(installed_path)?;
    let value: Value = serde_json::from_slice(&bytes).map_err(|error| {
        Problem::code(
            format!("installed.json 无效：{error}"),
            "agent_host_mismatch",
        )
    })?;
    let object = value
        .as_object()
        .ok_or_else(|| Problem::code("installed.json 必须为 JSON 对象。", "agent_host_mismatch"))?;
    let bin_dir = object
        .get("bin_dir")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| Problem::code("installed.json 缺少 bin_dir。", "agent_host_mismatch"))?;
    let data_dir = object
        .get("data_dir")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| Problem::code("installed.json 缺少 data_dir。", "agent_host_mismatch"))?;
    let parent = installed_path.parent().unwrap_or(Path::new("."));
    Ok(Some((
        resolve_path(bin_dir, parent)?,
        resolve_path(data_dir, parent)?,
    )))
}

fn installed_token_path(config: &Value) -> Option<PathBuf> {
    let installed = Path::new(config.get("installed")?.as_str()?);
    let bytes = std::fs::read(installed).ok()?;
    let value: Value = serde_json::from_slice(&bytes).ok()?;
    let data_dir = value.get("data_dir")?.as_str()?;
    let parent = installed.parent().unwrap_or(Path::new("."));
    Some(resolve_path(data_dir, parent).ok()?.join("admin.token"))
}

fn default_remote() -> Value {
    json!({
        "name": "远端设备",
        "node_id": null,
        "management_service_id": null,
        "ssh_host": "",
        "python": "python3",
        "installed": "~/.local/lib/p2p-file-node/installed.json",
    })
}

#[derive(Clone, Copy)]
enum RemoteOperation {
    Request,
    Probe,
}

fn remote_script(config: &Value, operation: RemoteOperation) -> Result<String> {
    let remote = config
        .get("remote")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            Problem::code(
                "未配置独立 SSH 管理通道；可选择设备已经发布的授权服务。",
                "remote_not_configured",
            )
        })?;
    let installed = remote
        .get("installed")
        .and_then(Value::as_str)
        .ok_or_else(|| Problem::new("请配置远端 installed.json 路径。"))?;
    let installed = serde_json::to_string(installed)?;
    let prelude = format!(
        concat!(
            "import json,os,socket,subprocess,sys\n",
            "from pathlib import Path\n",
            "i=json.loads(Path(os.path.expandvars({})).expanduser().read_text(encoding=\"utf-8\"))\n",
        ),
        installed
    );
    let body = match operation {
        RemoteOperation::Request => concat!(
            "p=subprocess.run([str(Path(i[\"bin_dir\"])/(\"p2p-node.exe\" if os.name==\"nt\" else \"p2p-node\")),\"--data-dir\",i[\"data_dir\"],\"request\",\"-\"],input=sys.stdin.read(),text=True,encoding=\"utf-8\",capture_output=True,timeout=35)\n",
            "sys.stdout.write(p.stdout)\n",
            "sys.exit(p.returncode)\n",
        ),
        RemoteOperation::Probe => concat!(
            "s=json.loads((Path(i[\"data_dir\"])/\"node.json\").read_text(encoding=\"utf-8\"))[\"settings\"]\n",
            "service_id=json.loads(sys.stdin.read())\n",
            "v=next(x for x in s[\"services\"] if x[\"id\"]==service_id)\n",
            "host,port=v[\"target\"].rsplit(\":\",1)\n",
            "try:\n",
            " c=socket.create_connection((host.strip(\"[]\"),int(port)),3);c.close();print(json.dumps({\"reachable\":True,\"target\":v[\"target\"]}))\n",
            "except OSError:\n",
            " print(json.dumps({\"reachable\":False,\"target\":v[\"target\"]}))\n",
        ),
    };
    Ok(prelude + body)
}

fn ssh_argv(config: &Value, script: &str) -> Result<Vec<String>> {
    let remote = config
        .get("remote")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            Problem::code(
                "未配置独立 SSH 管理通道；可选择设备已经发布的授权服务。",
                "remote_not_configured",
            )
        })?;
    let ssh_host = remote
        .get("ssh_host")
        .and_then(Value::as_str)
        .ok_or_else(|| Problem::new("请配置远端 SSH 主机或别名。"))?;
    let python = remote
        .get("python")
        .and_then(Value::as_str)
        .ok_or_else(|| Problem::new("请配置远端 Python。"))?;
    let payload = base64_encode(script.as_bytes());
    let bootstrap = format!("exec(__import__('base64').b64decode('{payload}'))");
    Ok(vec![
        "ssh".into(),
        "-o".into(),
        "BatchMode=yes".into(),
        "-o".into(),
        "ConnectTimeout=8".into(),
        ssh_host.into(),
        format!("{python} -c \"{bootstrap}\""),
    ])
}

struct ProcessOutput {
    success: bool,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

async fn run_process(argv: &[String], input: Option<&[u8]>) -> Result<ProcessOutput> {
    if argv.is_empty() {
        return Err(Problem::new("管理命令为空。"));
    }
    let mut command = ProcessCommand::new(&argv[0]);
    command
        .args(&argv[1..])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x08000000);
    let mut child = command.spawn().map_err(agent_unavailable)?;
    if let Some(input) = input {
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| Problem::unknown("无法写入远端管理请求。"))?;
        stdin.write_all(input).await.map_err(agent_unavailable)?;
        stdin.shutdown().await.map_err(agent_unavailable)?;
    }
    let output = tokio::time::timeout(MANAGED_REMOTE_COMMAND_TIMEOUT, child.wait_with_output())
        .await
        .map_err(|_| Problem::unknown("管理请求超时，请刷新状态确认结果，不要重复提交。"))?
        .map_err(agent_unavailable)?;
    Ok(ProcessOutput {
        success: output.status.success(),
        stdout: output.stdout,
        stderr: output.stderr,
    })
}

fn remote_result(output: &ProcessOutput) -> Result<Value> {
    let value: Value = serde_json::from_slice(&output.stdout)
        .map_err(|_| Problem::unknown("远端管理通道未返回有效响应，请刷新状态确认结果。"))?;
    if let Some(error) = value.get("error").filter(|error| !error.is_null()) {
        let code = error
            .get("code")
            .and_then(Value::as_str)
            .unwrap_or("agent_error");
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("Agent 拒绝了操作");
        return Err(problem_for_code(code, message));
    }
    if !output.success {
        let detail = String::from_utf8_lossy(&output.stderr);
        return Err(Problem::unknown(format!(
            "远端管理进程未正常完成：{}",
            detail.chars().take(512).collect::<String>()
        )));
    }
    Ok(value.get("result").cloned().unwrap_or(value))
}

fn response_result(response: ipc::Response) -> Result<Value> {
    if let Some(error) = response.error {
        return Err(problem_for_code(&error.code, &error.message));
    }
    response
        .result
        .ok_or_else(|| Problem::unknown("Agent 未返回管理结果，请刷新状态确认。"))
}

fn problem_for_code(code: &str, message: &str) -> Problem {
    let mut problem = Problem::code(message, code);
    problem.uncertain = matches!(
        code,
        "outcome_unknown" | "agent_unavailable" | "recovery_required"
    );
    problem
}

fn agent_unavailable(error: impl ToString) -> Problem {
    let mut problem = Problem::code(
        format!("管理程序不可用：{}", error.to_string()),
        "agent_unavailable",
    );
    problem.uncertain = true;
    problem
}

fn current_platform() -> Result<&'static str> {
    match std::env::consts::OS {
        "macos" => Ok("macos"),
        "windows" => Ok("windows"),
        "linux" => Ok("linux"),
        _ => Err(platform_problem()),
    }
}

fn platform_problem() -> Problem {
    Problem::new("导航管理台支持 macOS、Windows 和 Linux。")
}

fn config_string<'a>(config: &'a Value, key: &str) -> Result<&'a str> {
    config
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| Problem::new(format!("{key} 必须为字符串。")))
}

fn valid_service_name(value: &str) -> bool {
    (1..=180).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_.-".contains(&byte))
}

fn is_node_id(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn loopback_http_url(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    let rest = if let Some(rest) = lower.strip_prefix("http://") {
        rest
    } else if let Some(rest) = lower.strip_prefix("https://") {
        rest
    } else {
        return false;
    };
    if (value.contains('?') || value.contains('#')) || rest.is_empty() || rest.starts_with('/') {
        return false;
    }
    let authority = rest.split('/').next().unwrap_or_default();
    if authority.contains('@') {
        return false;
    }
    let host = if let Some(after) = authority.strip_prefix('[') {
        let Some((host, remainder)) = after.split_once(']') else {
            return false;
        };
        if host != "::1"
            || (!remainder.is_empty() && !remainder.strip_prefix(':').is_some_and(valid_port_text))
        {
            return false;
        }
        host
    } else {
        let (host, port) = authority
            .split_once(':')
            .map_or((authority, None), |(host, port)| (host, Some(port)));
        if !matches!(host, "127.0.0.1" | "localhost") {
            return false;
        }
        if port.is_some_and(|port| !valid_port_text(port)) {
            return false;
        }
        host
    };
    !host.is_empty()
}

fn valid_port_text(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| byte.is_ascii_digit())
        && value.parse::<u16>().is_ok_and(|port| port >= 1)
}

fn resolve_path(value: &str, parent: &Path) -> Result<PathBuf> {
    let expanded = expand_path(value)?;
    let path = if expanded.is_absolute() {
        expanded
    } else {
        parent.join(expanded)
    };
    absolute_path(&path)
}

fn absolute_path(path: &Path) -> Result<PathBuf> {
    std::path::absolute(path)
        .map_err(|error| Problem::new(format!("无法规范化路径 {}：{error}", path.display())))
}

fn expand_path(value: &str) -> Result<PathBuf> {
    let mut text = value.to_owned();
    if matches!(text.as_str(), "~") || text.starts_with("~/") || text.starts_with("~\\") {
        let home = dirs::home_dir().ok_or_else(|| Problem::new("无法确定当前用户主目录。"))?;
        text = if text.len() == 1 {
            path_text(&home)?.to_owned()
        } else {
            path_text(&home.join(&text[2..]))?.to_owned()
        };
    }
    Ok(PathBuf::from(expand_environment(&text)))
}

fn expand_environment(value: &str) -> String {
    let mut output = String::new();
    let mut rest = value;
    while let Some(index) = rest.find(|character| character == '$' || character == '%') {
        output.push_str(&rest[..index]);
        let candidate = &rest[index..];
        let parsed = if candidate.starts_with("${") {
            candidate[2..]
                .find('}')
                .map(|end| (&candidate[2..2 + end], 3 + end))
        } else if candidate.starts_with('$') {
            let end = candidate[1..]
                .find(|character: char| !character.is_ascii_alphanumeric() && character != '_')
                .map_or(candidate.len(), |end| end + 1);
            (end > 1).then_some((&candidate[1..end], end))
        } else if candidate.starts_with('%') {
            candidate[1..]
                .find('%')
                .map(|end| (&candidate[1..1 + end], end + 2))
        } else {
            None
        };
        if let Some((name, consumed)) = parsed {
            if let Some(expanded) = std::env::var_os(name) {
                output.push_str(&expanded.to_string_lossy());
            } else {
                output.push_str(&candidate[..consumed]);
            }
            rest = &candidate[consumed..];
        } else {
            let character = candidate.chars().next().unwrap_or_default();
            output.push(character);
            rest = &candidate[character.len_utf8()..];
        }
    }
    output.push_str(rest);
    output
}

fn path_text(path: &Path) -> Result<&str> {
    path.to_str()
        .ok_or_else(|| Problem::new("导航配置路径必须为 UTF-8。"))
}

fn same_path(left: &Path, right: &Path) -> bool {
    #[cfg(windows)]
    {
        left.to_string_lossy()
            .eq_ignore_ascii_case(&right.to_string_lossy())
    }
    #[cfg(not(windows))]
    {
        left == right
    }
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn base64_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let value = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        output.push(ALPHABET[((value >> 18) & 63) as usize] as char);
        output.push(ALPHABET[((value >> 12) & 63) as usize] as char);
        output.push(if chunk.len() > 1 {
            ALPHABET[((value >> 6) & 63) as usize] as char
        } else {
            '='
        });
        output.push(if chunk.len() > 2 {
            ALPHABET[(value & 63) as usize] as char
        } else {
            '='
        });
    }
    output
}

#[cfg(test)]
#[path = "platform_tests.rs"]
mod tests;
