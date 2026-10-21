//! Optional loopback gateway. Owns only an IPC client and an HTTP token.
use super::{agent_client::AgentClient, agent_paths::AgentPaths, ipc};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Request, State},
    http::StatusCode,
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use std::{io, net::SocketAddr, sync::Arc, time::Duration};
use tokio::sync::Semaphore;

#[derive(Clone)]
struct Gateway {
    client: AgentClient,
    token: Arc<zeroize::Zeroizing<String>>,
    slots: Arc<Semaphore>,
}
pub async fn run(paths: AgentPaths, listen: SocketAddr) -> io::Result<()> {
    if !listen.ip().is_loopback() {
        return Err(io::Error::other("Web管理只允许loopback监听"));
    }
    let token_path = paths.root.join("admin.token");
    if let Ok(metadata) = std::fs::symlink_metadata(&token_path) {
        if !crate::storage::atomic::regular_file(&metadata) || metadata.len() > 4096 {
            return Err(io::Error::other("token必须是私有普通文件"));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            if metadata.uid() != rustix::process::getuid().as_raw()
                || metadata.permissions().mode() & 0o077 != 0
            {
                return Err(io::Error::other("token文件必须属于当前用户且权限0600"));
            }
        }
    }
    let token = crate::admin::load_or_create_token(&token_path).map_err(io::Error::other)?;
    let state = Gateway {
        client: AgentClient::new(paths),
        token: Arc::new(zeroize::Zeroizing::new(token)),
        slots: Arc::new(Semaphore::new(32)),
    };
    let app = Router::new()
        .route("/", get(index))
        .route("/api/v1/request", post(request))
        .layer(DefaultBodyLimit::max(256 * 1024))
        .layer(middleware::from_fn_with_state(state.clone(), boundary))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind(listen).await?;
    eprintln!(
        "Web管理：http://{}；token文件：{}",
        listener.local_addr()?,
        token_path.display()
    );
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
}
async fn boundary(State(state): State<Gateway>, request: Request, next: Next) -> Response {
    let Ok(_permit) = state.slots.clone().try_acquire_owned() else {
        return (StatusCode::TOO_MANY_REQUESTS, "管理请求并发已满").into_response();
    };
    if request.uri().path().starts_with("/api/")
        && let Err(response) = crate::admin::authorize_management(request.headers(), &state.token)
    {
        return *response;
    }
    match tokio::time::timeout(Duration::from_secs(35), next.run(request)).await {
        Ok(response) => response,
        Err(_) => (StatusCode::GATEWAY_TIMEOUT, "请求超时，变更结果须查询确认").into_response(),
    }
}
async fn request(State(state): State<Gateway>, Json(request): Json<ipc::Request>) -> Response {
    let request_id = request.request_id.clone();
    match state.client.request(request).await {
        Ok(response) => {
            let status = match response.error.as_ref().map(|e| e.code.as_str()) {
                Some("revision_conflict" | "id_conflict") => StatusCode::CONFLICT,
                Some(_) => StatusCode::BAD_REQUEST,
                None => StatusCode::OK,
            };
            (status, Json(response)).into_response()
        }
        Err(error) => (
            StatusCode::BAD_GATEWAY,
            Json(ipc::Response::error(
                &request_id,
                "agent_unavailable",
                error,
            )),
        )
            .into_response(),
    }
}
fn node_platform() -> &'static str {
    if std::env::consts::OS == "linux"
        && std::fs::read_to_string("/etc/os-release").is_ok_and(|release| {
            release
                .lines()
                .any(|line| matches!(line, "ID=ubuntu" | "ID=\"ubuntu\""))
        })
    {
        "ubuntu"
    } else {
        std::env::consts::OS
    }
}

fn index_html(platform: &str) -> String {
    let (name, scope, theme, purpose) = match platform {
        "macos" => (
            "Mac",
            "本机 Mac",
            "mac",
            "在这台 Mac 上接收远程服务，配置本机监听端口。",
        ),
        "ubuntu" => (
            "Ubuntu",
            "Ubuntu 服务器",
            "server",
            "管理 Ubuntu 提供的服务、目标地址与允许访问的设备。",
        ),
        "linux" => (
            "Linux",
            "Linux 服务器",
            "server",
            "管理 Linux 提供的服务、目标地址与允许访问的设备。",
        ),
        "windows" => (
            "Windows",
            "Windows 节点",
            "windows",
            "管理这台 Windows 的服务、端口转发与文件任务。",
        ),
        _ => (
            "当前节点",
            "当前节点",
            "default",
            "管理当前后台节点的服务、端口转发与文件任务。",
        ),
    };
    include_str!("agent_web.html")
        .replace("__NODE_NAME__", name)
        .replace("__NODE_SCOPE__", scope)
        .replace("__NODE_THEME__", theme)
        .replace("__NODE_PURPOSE__", purpose)
}

async fn index() -> Response {
    let mut response = Html(index_html(node_platform())).into_response();
    for (name, value) in [
        (
            "content-security-policy",
            "default-src 'none'; style-src 'unsafe-inline'; script-src 'unsafe-inline'; connect-src 'self'; base-uri 'none'; frame-ancestors 'none'; form-action 'none'",
        ),
        ("x-frame-options", "DENY"),
        ("x-content-type-options", "nosniff"),
        ("referrer-policy", "no-referrer"),
        ("cache-control", "no-store"),
    ] {
        response.headers_mut().insert(
            axum::http::HeaderName::from_static(name),
            axum::http::HeaderValue::from_static(value),
        );
    }
    response
}
