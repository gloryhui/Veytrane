//! Loopback-only web administration for the CLI.
//!
//! The page edits the same JSON files the CLI already uses. The HTTP server is
//! deliberately small: static page, token-authenticated JSON API, no CORS and
//! loopback bind only. Remote administration is expected to go through an SSH
//! tunnel.

use std::io::{self, Write};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::extract::{DefaultBodyLimit, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Serialize;
use serde_json::{Value, json};
use tokio::sync::{Mutex, RwLock};
use tracing::{info, warn};

use crate::error::{Error, Result};
use crate::identity::NodeId;
use crate::tunnel::rules::{ClientConfig, ServePolicy, ServiceConfig};
use crate::tunnel::supervisor::RuleSupervisor;

const INDEX: &str = include_str!("index.html");
const MAX_BODY_BYTES: usize = 256 * 1024;

/// A running admin server. Dropping the handle detaches the task; the process
/// runtime owns it until the CLI exits.
pub struct AdminHandle {
    pub addr: SocketAddr,
    pub task: tokio::task::JoinHandle<()>,
}

/// State shared by the HTTP handlers.
#[derive(Clone)]
pub struct AdminState {
    inner: Arc<AdminInner>,
}

struct AdminInner {
    backend: Backend,
    peer: NodeId,
    config_path: PathBuf,
    token: String,
    last_error: RwLock<Option<String>>,
    op_lock: Mutex<()>,
}

#[derive(Clone)]
enum Backend {
    Service {
        policy: Arc<ServePolicy>,
        config: Arc<RwLock<ServiceConfig>>,
    },
    Client {
        supervisor: Arc<Mutex<RuleSupervisor>>,
        config: Arc<RwLock<ClientConfig>>,
    },
}

impl Backend {
    fn role(&self) -> &'static str {
        match self {
            Self::Service { .. } => "service",
            Self::Client { .. } => "client",
        }
    }
}

impl AdminState {
    pub fn service(
        policy: Arc<ServePolicy>,
        config: Arc<RwLock<ServiceConfig>>,
        peer: NodeId,
        config_path: PathBuf,
        token: String,
    ) -> Self {
        Self {
            inner: Arc::new(AdminInner {
                backend: Backend::Service { policy, config },
                peer,
                config_path,
                token,
                last_error: RwLock::new(None),
                op_lock: Mutex::new(()),
            }),
        }
    }

    pub fn client(
        supervisor: Arc<Mutex<RuleSupervisor>>,
        config: Arc<RwLock<ClientConfig>>,
        peer: NodeId,
        config_path: PathBuf,
        token: String,
    ) -> Self {
        Self {
            inner: Arc::new(AdminInner {
                backend: Backend::Client { supervisor, config },
                peer,
                config_path,
                token,
                last_error: RwLock::new(None),
                op_lock: Mutex::new(()),
            }),
        }
    }
}

/// Bind and spawn the admin server. Only loopback addresses are accepted.
pub async fn spawn(listen: SocketAddr, state: AdminState) -> Result<AdminHandle> {
    if state.inner.token.len() < 32 {
        return Err(Error::Protocol("Web 管理必须配置独立的有效 token".into()));
    }
    if !listen.ip().is_loopback() {
        return Err(Error::Protocol(format!(
            "Web 管理只能监听 loopback 地址（127.0.0.1 或 ::1），收到 {listen}；远程访问请用 SSH 隧道"
        )));
    }
    let listener = tokio::net::TcpListener::bind(listen).await?;
    let addr = listener.local_addr()?;
    let app = router(state);
    let task = tokio::spawn(async move {
        if let Err(error) = axum::serve(listener, app).await {
            warn!(%error, "Web 管理服务退出");
        }
    });
    info!(%addr, "Web 管理页已启动");
    Ok(AdminHandle { addr, task })
}

fn router(state: AdminState) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/api/v1/status", get(get_status))
        .route("/api/v1/config", get(get_config).put(put_config))
        .route("/api/v1/reload", post(reload))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .with_state(state)
}

async fn index() -> Response {
    let mut response = Html(INDEX).into_response();
    let headers = response.headers_mut();
    headers.insert(
        HeaderName::from_static("content-security-policy"),
        HeaderValue::from_static(
            "default-src 'none'; style-src 'unsafe-inline'; script-src 'unsafe-inline'; connect-src 'self'; img-src 'self' data:; base-uri 'none'; form-action 'none'; frame-ancestors 'none'",
        ),
    );
    headers.insert(
        HeaderName::from_static("x-frame-options"),
        HeaderValue::from_static("DENY"),
    );
    headers.insert(
        HeaderName::from_static("x-content-type-options"),
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        HeaderName::from_static("referrer-policy"),
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

async fn get_status(
    State(state): State<AdminState>,
    headers: HeaderMap,
) -> std::result::Result<Json<Value>, ApiError> {
    authorize(&state, &headers)?;
    let _operation = state.inner.op_lock.lock().await;
    let mut body = json!({
        "role": state.inner.backend.role(),
        "peer": state.inner.peer.to_hex(),
        "config_path": state.inner.config_path.display().to_string(),
        "last_error": *state.inner.last_error.read().await,
    });
    match &state.inner.backend {
        Backend::Service { config, .. } => {
            body["targets"] = serde_json::to_value(&config.read().await.forward_targets)
                .map_err(|error| ApiError::internal(error.to_string()))?;
        }
        Backend::Client { supervisor, config } => {
            body["rules"] = serde_json::to_value(&config.read().await.rules)
                .map_err(|error| ApiError::internal(error.to_string()))?;
            let listeners = supervisor
                .lock()
                .await
                .listeners()
                .into_iter()
                .map(|(id, listen)| json!({"id": id, "listen": listen}))
                .collect::<Vec<_>>();
            body["listeners"] = Value::Array(listeners);
        }
    }
    Ok(Json(body))
}

async fn get_config(
    State(state): State<AdminState>,
    headers: HeaderMap,
) -> std::result::Result<Json<Value>, ApiError> {
    authorize(&state, &headers)?;
    let _operation = state.inner.op_lock.lock().await;
    let config = match &state.inner.backend {
        Backend::Service { config, .. } => serde_json::to_value(&*config.read().await)
            .map_err(|error| ApiError::internal(error.to_string()))?,
        Backend::Client { config, .. } => serde_json::to_value(&*config.read().await)
            .map_err(|error| ApiError::internal(error.to_string()))?,
    };
    Ok(Json(json!({
        "role": state.inner.backend.role(),
        "peer": state.inner.peer.to_hex(),
        "config_path": state.inner.config_path.display().to_string(),
        "config": config,
    })))
}

async fn put_config(
    State(state): State<AdminState>,
    headers: HeaderMap,
    body: std::result::Result<Json<Value>, axum::extract::rejection::JsonRejection>,
) -> std::result::Result<Json<Value>, ApiError> {
    authorize(&state, &headers)?;
    let _operation = state.inner.op_lock.lock().await;
    let result = async {
        let Json(body) = body.map_err(|error| ApiError {
            status: error.status(),
            message: error.body_text(),
            details: Vec::new(),
        })?;
        match &state.inner.backend {
            Backend::Service { .. } => {
                let next: ServiceConfig = serde_json::from_value(body)
                    .map_err(|error| ApiError::bad_request(format!("服务配置格式错误：{error}")))?;
                validate_service_peer(&state, &next, true)?;
                apply_service(&state, next, true).await
            }
            Backend::Client { .. } => {
                let next: ClientConfig = serde_json::from_value(body).map_err(|error| {
                    ApiError::bad_request(format!("客户端配置格式错误：{error}"))
                })?;
                validate_client_peer(&state, &next, true)?;
                apply_client(&state, next, true).await
            }
        }
    }
    .await;
    record_result(&state, &result).await;
    result?;
    Ok(Json(json!({"ok": true})))
}

async fn reload(
    State(state): State<AdminState>,
    headers: HeaderMap,
) -> std::result::Result<Json<Value>, ApiError> {
    authorize(&state, &headers)?;
    reload_config(&state, true).await?;
    Ok(Json(json!({"ok": true})))
}

impl AdminState {
    /// Local SIGHUP shares exactly the HTTP coordinator, including its error state.
    /// Empty enabled sets are permitted locally for complete revocation.
    pub async fn reload_local(&self) -> Result<()> {
        reload_config(self, false).await.map_err(|error| {
            Error::Protocol(format!("{}：{}", error.message, error.details.join("; ")))
        })
    }
}

async fn reload_config(
    state: &AdminState,
    require_enabled: bool,
) -> std::result::Result<(), ApiError> {
    let _operation = state.inner.op_lock.lock().await;
    let result = async {
        match &state.inner.backend {
            Backend::Service { .. } => {
                let next = ServiceConfig::load(&state.inner.config_path)
                    .map_err(|error| ApiError::bad_request(error.to_string()))?;
                validate_service_peer(state, &next, require_enabled)?;
                apply_service(state, next, false).await
            }
            Backend::Client { .. } => {
                let next = ClientConfig::load(&state.inner.config_path)
                    .map_err(|error| ApiError::bad_request(error.to_string()))?;
                validate_client_peer(state, &next, require_enabled)?;
                apply_client(state, next, false).await
            }
        }
    }
    .await;
    record_result(state, &result).await;
    result
}

async fn record_result(state: &AdminState, result: &std::result::Result<(), ApiError>) {
    *state.inner.last_error.write().await = result.as_ref().err().map(|error| {
        if error.details.is_empty() {
            error.message.clone()
        } else {
            format!("{}：{}", error.message, error.details.join("; "))
        }
    });
}

fn validate_service_peer(
    state: &AdminState,
    config: &ServiceConfig,
    require_enabled: bool,
) -> std::result::Result<(), ApiError> {
    if let Err(error) = config.validate() {
        return Err(ApiError::bad_request(error.to_string()));
    }
    let peers = config.peers();
    if require_enabled && peers.is_empty() {
        return Err(ApiError::bad_request(
            "Web 保存至少保留一个启用的目标，否则重启 serve 时无法确定对端",
        ));
    }
    if peers.len() > 1 || peers.first().is_some_and(|peer| *peer != state.inner.peer) {
        return Err(ApiError::bad_request(
            "serve 当前只服务一个对端；不能通过 Web 切换对端，请重启进程",
        ));
    }
    Ok(())
}

fn validate_client_peer(
    state: &AdminState,
    config: &ClientConfig,
    require_enabled: bool,
) -> std::result::Result<(), ApiError> {
    if let Err(error) = config.validate() {
        return Err(ApiError::bad_request(error.to_string()));
    }
    let peers = config.peers();
    if require_enabled && peers.is_empty() {
        return Err(ApiError::bad_request(
            "Web 保存至少保留一条启用的转发规则，否则重启 forward 时无法确定对端",
        ));
    }
    if peers.len() > 1 || peers.first().is_some_and(|peer| *peer != state.inner.peer) {
        return Err(ApiError::bad_request(
            "所有启用规则必须共享当前对端；切换对端需要重启 forward",
        ));
    }
    Ok(())
}

async fn apply_service(
    state: &AdminState,
    next: ServiceConfig,
    persist: bool,
) -> std::result::Result<(), ApiError> {
    let Backend::Service { policy, config } = &state.inner.backend else {
        return Err(ApiError::internal("角色与请求不匹配"));
    };
    let mut effective = config.write().await;
    let mut last_error = state.inner.last_error.write().await;
    let prepared = policy
        .prepare_replace(&next)
        .map_err(|error| ApiError::internal(error.to_string()))?;
    let outcome = if persist {
        write_json_atomic(&state.inner.config_path, &next)
            .map_err(|error| ApiError::internal(format!("写回失败，保留原配置与策略：{error}")))?
    } else {
        Default::default()
    };
    // All async locks and resources are ready before the file replacement. No
    // await is permitted between replacement and effective state publication.
    prepared.commit();
    *effective = next;
    *last_error = outcome.durability_warning.clone();
    match outcome.durability_warning {
        Some(message) => Err(ApiError::internal(message)),
        None => Ok(()),
    }
}

async fn apply_client(
    state: &AdminState,
    next: ClientConfig,
    persist: bool,
) -> std::result::Result<(), ApiError> {
    let Backend::Client { supervisor, config } = &state.inner.backend else {
        return Err(ApiError::internal("角色与请求不匹配"));
    };
    let mut supervisor = supervisor.lock().await;
    let prepared = supervisor
        .prepare(&next.enabled_rules())
        .await
        .map_err(|error| {
            ApiError::conflict("规则准备失败，保留全部原规则", vec![error.to_string()])
        })?;
    let mut effective = config.write().await;
    let mut last_error = state.inner.last_error.write().await;
    let outcome = if persist {
        write_json_atomic(&state.inner.config_path, &next)
            .map_err(|error| ApiError::internal(format!("写回失败，保留全部原规则：{error}")))?
    } else {
        Default::default()
    };
    let stopped = supervisor.commit(prepared);
    *effective = next;
    *last_error = outcome.durability_warning.clone();
    drop(effective);
    drop(last_error);
    drop(supervisor);
    for task in stopped {
        let _ = task.await;
    }
    match outcome.durability_warning {
        Some(message) => Err(ApiError::internal(message)),
        None => Ok(()),
    }
}

fn authorize(state: &AdminState, headers: &HeaderMap) -> std::result::Result<(), ApiError> {
    authorize_token(headers, &state.inner.token)
}
#[cfg(feature = "node")]
pub(crate) fn authorize_management(
    headers: &HeaderMap,
    token: &str,
) -> std::result::Result<(), Box<Response>> {
    authorize_token(headers, token).map_err(|error| Box::new(error.into_response()))
}
fn authorize_token(headers: &HeaderMap, expected: &str) -> std::result::Result<(), ApiError> {
    let host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .ok_or_else(|| ApiError::bad_request("缺少 Host 请求头"))?;
    if !host_allowed(host) {
        return Err(ApiError::forbidden("Host 不受信任"));
    }
    if let Some(origin) = headers.get(header::ORIGIN) {
        let origin = origin
            .to_str()
            .map_err(|_| ApiError::forbidden("Origin 非法"))?;
        if origin != format!("http://{host}") {
            return Err(ApiError::forbidden("Origin 与 Host 不匹配"));
        }
    }
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .ok_or_else(ApiError::unauthorized)?
        .trim();
    if !constant_time_eq(token.as_bytes(), expected.as_bytes()) {
        return Err(ApiError::unauthorized());
    }
    Ok(())
}

fn host_allowed(host: &str) -> bool {
    // Allow any local port so an SSH tunnel may map a different local port.
    // The hostname itself must stay loopback; DNS rebinding uses a foreign name.
    let hostname = if host.starts_with('[') {
        host.split_once(']')
            .map(|(name, _)| format!("{name}]"))
            .unwrap_or_else(|| host.to_string())
    } else {
        host.rsplit_once(':')
            .map(|(name, _)| name.to_string())
            .unwrap_or_else(|| host.to_string())
    };
    matches!(hostname.as_str(), "127.0.0.1" | "localhost" | "[::1]")
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut difference = 0_u8;
    for (a, b) in left.iter().zip(right) {
        difference |= a ^ b;
    }
    difference == 0
}

fn write_json_atomic<T: Serialize>(
    path: &Path,
    value: &T,
) -> io::Result<crate::storage::atomic::WriteOutcome> {
    crate::storage::atomic::write_json(path, value)
}

/// Default token location next to the edited JSON file.
pub fn default_token_path(config_path: &Path) -> PathBuf {
    let file_name = config_path
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or("p2p");
    config_path.with_file_name(format!("{file_name}.admin-token"))
}

/// Read a token file, creating a random token with `0600` permissions if absent.
pub fn load_or_create_token(path: &Path) -> Result<String> {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            let token = text.trim().to_string();
            if token.len() < 32 {
                return Err(Error::Protocol(format!(
                    "管理 token 文件 {} 内容过短（至少 32 字符）",
                    path.display()
                )));
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Ok(metadata) = std::fs::metadata(path)
                    && metadata.permissions().mode() & 0o077 != 0
                {
                    warn!(
                        path = %path.display(),
                        "管理 token 文件权限过宽，建议 chmod 600"
                    );
                }
            }
            Ok(token)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            if let Some(parent) = path.parent()
                && !parent.as_os_str().is_empty()
            {
                std::fs::create_dir_all(parent)?;
            }
            let token = hex::encode(rand::random::<[u8; 32]>());
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(path)?;
            file.write_all(token.as_bytes())?;
            file.write_all(b"\n")?;
            file.sync_all()?;
            Ok(token)
        }
        Err(error) => Err(Error::Io(io::Error::new(
            error.kind(),
            format!("读取管理 token 文件 {} 失败: {error}", path.display()),
        ))),
    }
}

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    message: String,
    details: Vec<String>,
}

impl ApiError {
    fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
            details: Vec::new(),
        }
    }
    fn unauthorized() -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            message: "需要有效的管理 token".into(),
            details: Vec::new(),
        }
    }
    fn forbidden(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            message: message.into(),
            details: Vec::new(),
        }
    }
    fn conflict(message: impl Into<String>, details: Vec<String>) -> Self {
        Self {
            status: StatusCode::CONFLICT,
            message: message.into(),
            details,
        }
    }
    fn internal(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: message.into(),
            details: Vec::new(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = Json(json!({
            "error": self.message,
            "details": self.details,
        }));
        let mut response = (self.status, body).into_response();
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        response
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use axum::body::{Body, to_bytes};
    use axum::http::Request;
    use tower::ServiceExt;

    use crate::tunnel::rules::RULES_SCHEMA_VERSION;

    fn node_id(seed: u8) -> NodeId {
        NodeId::from_hex(&format!("{seed:02x}").repeat(16)).unwrap()
    }

    fn service_fixture(target: &str) -> (AdminState, Arc<ServePolicy>, PathBuf, NodeId) {
        let peer = node_id(1);
        let config = ServiceConfig {
            version: RULES_SCHEMA_VERSION,
            forward_targets: vec![crate::tunnel::rules::AllowedForwardTarget {
                id: "sub2api".into(),
                name: "Sub2API".into(),
                target: target.parse().unwrap(),
                enabled: true,
                allowed_peers: vec![peer.to_hex()],
            }],
        };
        let policy = Arc::new(ServePolicy::new(&config).unwrap());
        let directory = std::env::temp_dir().join(format!("p2p-admin-{}", rand::random::<u128>()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("service.json");
        write_json_atomic(&path, &config).unwrap();
        let state = AdminState::service(
            policy.clone(),
            Arc::new(RwLock::new(config)),
            peer,
            path.clone(),
            "test-token-0123456789abcdef0123456789abcdef".into(),
        );
        (state, policy, path, peer)
    }

    fn request(method: &str, uri: &str, token: Option<&str>) -> Request<Body> {
        let mut builder = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::HOST, "127.0.0.1:7788");
        if let Some(token) = token {
            builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        builder.body(Body::empty()).unwrap()
    }

    #[tokio::test]
    async fn static_page_is_public_and_api_requires_token() {
        let (state, _, _, _) = service_fixture("127.0.0.1:8080");
        let app = router(state.clone());
        let response = app
            .clone()
            .oneshot(request("GET", "/", None))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        assert!(String::from_utf8_lossy(&body).contains("p2p_file"));

        let response = app
            .oneshot(request("GET", "/api/v1/status", None))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn token_and_host_are_enforced() {
        let (state, _, _, _) = service_fixture("127.0.0.1:8080");
        let token = "test-token-0123456789abcdef0123456789abcdef";
        let app = router(state);
        let response = app
            .clone()
            .oneshot(request("GET", "/api/v1/status", Some(token)))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let response = app
            .clone()
            .oneshot(request("GET", "/api/v1/status", Some("wrong-token")))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        // SSH tunnels may map the admin port to a different local port.
        let tunnel_host = Request::builder()
            .uri("/api/v1/status")
            .header(header::HOST, "127.0.0.1:18000")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(tunnel_host).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let cross_origin = Request::builder()
            .uri("/api/v1/status")
            .header(header::HOST, "127.0.0.1:7788")
            .header(header::ORIGIN, "http://evil.example")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(cross_origin).await.unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);

        let bad_host = Request::builder()
            .uri("/api/v1/status")
            .header(header::HOST, "evil.example:7788")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(bad_host).await.unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn valid_update_applies_policy_and_persists() {
        let (state, policy, path, peer) = service_fixture("127.0.0.1:8080");
        let token = "test-token-0123456789abcdef0123456789abcdef";
        let mut next = ServiceConfig::load(&path).unwrap();
        next.forward_targets[0].target = "127.0.0.1:9090".parse().unwrap();
        let body = serde_json::to_vec(&next).unwrap();
        let request = Request::builder()
            .method("PUT")
            .uri("/api/v1/config")
            .header(header::HOST, "127.0.0.1:7788")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body))
            .unwrap();
        let response = router(state.clone()).oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(policy.target_allowed(peer, "127.0.0.1:9090".parse().unwrap()));
        assert!(!policy.target_allowed(peer, "127.0.0.1:8080".parse().unwrap()));
        let saved: ServiceConfig = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(
            saved.forward_targets[0].target,
            "127.0.0.1:9090".parse().unwrap()
        );
    }

    #[tokio::test]
    async fn invalid_update_keeps_policy_and_file() {
        let (state, policy, path, peer) = service_fixture("127.0.0.1:8080");
        let token = "test-token-0123456789abcdef0123456789abcdef";
        let before = std::fs::read(&path).unwrap();
        let body = br#"{"version":1,"forward_targets":[{"id":"bad","name":"bad","target":"0.0.0.0:80","enabled":true,"allowed_peers":[]}]}"#;
        let request = Request::builder()
            .method("PUT")
            .uri("/api/v1/config")
            .header(header::HOST, "127.0.0.1:7788")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(&body[..]))
            .unwrap();
        let response = router(state).oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(policy.target_allowed(peer, "127.0.0.1:8080".parse().unwrap()));
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }

    #[tokio::test]
    async fn empty_web_config_is_rejected_to_keep_restart_viable() {
        let (state, policy, path, peer) = service_fixture("127.0.0.1:8080");
        let token = "test-token-0123456789abcdef0123456789abcdef";
        let empty = ServiceConfig {
            version: RULES_SCHEMA_VERSION,
            forward_targets: vec![],
        };
        let body = serde_json::to_vec(&empty).unwrap();
        let request = Request::builder()
            .method("PUT")
            .uri("/api/v1/config")
            .header(header::HOST, "127.0.0.1:7788")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body))
            .unwrap();
        let response = router(state).oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(policy.target_allowed(peer, "127.0.0.1:8080".parse().unwrap()));
        assert!(ServiceConfig::load(&path).is_ok());
    }

    #[tokio::test]
    async fn reload_reads_file_and_applies_policy() {
        let (state, policy, path, peer) = service_fixture("127.0.0.1:8080");
        let token = "test-token-0123456789abcdef0123456789abcdef";
        let mut next = ServiceConfig::load(&path).unwrap();
        next.forward_targets[0].target = "127.0.0.1:9091".parse().unwrap();
        write_json_atomic(&path, &next).unwrap();

        let request = Request::builder()
            .method("POST")
            .uri("/api/v1/reload")
            .header(header::HOST, "127.0.0.1:7788")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap();
        let response = router(state).oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(policy.target_allowed(peer, "127.0.0.1:9091".parse().unwrap()));
    }

    #[test]
    fn token_file_is_created_once_with_private_permissions() {
        let directory = std::env::temp_dir().join(format!("p2p-token-{}", rand::random::<u128>()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("admin.token");
        let first = load_or_create_token(&path).unwrap();
        let second = load_or_create_token(&path).unwrap();
        assert_eq!(first, second);
        assert_eq!(first.len(), 64);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o077,
                0
            );
        }
        let _ = std::fs::remove_dir_all(directory);
    }
}
