use super::{Backend, Problem, id, manager::Manager, platform};
use crate::node::local::paths::AgentPaths;
use axum::{
    Json, Router,
    extract::{ConnectInfo, DefaultBodyLimit, Request, State},
    http::{HeaderName, HeaderValue, Method, StatusCode},
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use serde_json::{Value, json};
use std::{collections::HashSet, io, net::SocketAddr, path::Path, sync::Arc};
use tokio::sync::{Mutex, Semaphore, watch};

#[derive(Clone)]
struct Gateway {
    manager: Arc<Mutex<Manager>>,
    page: std::path::PathBuf,
    host: Value,
    token: Arc<zeroize::Zeroizing<String>>,
    origins: Arc<HashSet<String>>,
    hosts: Arc<HashSet<String>>,
    slots: Arc<Semaphore>,
}

const PAGE_START: &str = "<script id=\"navigation-data\" type=\"application/json\">";
const PAGE_END: &str = "</script>";
const TEMPLATE: &str = include_str!("../../../../service-navigation.html");

pub(super) fn read_page_data(page: &Path) -> super::Result<Value> {
    let text = std::fs::read_to_string(page)?;
    let start = text
        .find(PAGE_START)
        .ok_or_else(|| Problem::new("导航数据块缺失。"))?
        + PAGE_START.len();
    let tail = &text[start..];
    let end = tail
        .find(PAGE_END)
        .ok_or_else(|| Problem::new("导航数据块不完整。"))?;
    Ok(serde_json::from_str(&tail[..end])?)
}

pub(super) fn replace_page_data(text: &str, data: &Value) -> super::Result<String> {
    let start = text
        .find(PAGE_START)
        .ok_or_else(|| Problem::new("导航数据块缺失。"))?
        + PAGE_START.len();
    let end = start
        + text[start..]
            .find(PAGE_END)
            .ok_or_else(|| Problem::new("导航数据块不完整。"))?;
    let encoded = serde_json::to_string_pretty(data)?
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('&', "\\u0026");
    Ok(format!("{}\n{encoded}\n{}", &text[..start], &text[end..]))
}

fn ensure_page(config: &Value, host: &Value) -> super::Result<()> {
    let page = Path::new(
        config["page"]
            .as_str()
            .ok_or_else(|| Problem::new("请配置私有导航页。"))?,
    );
    if page.try_exists()? {
        let metadata = std::fs::symlink_metadata(page)?;
        if !crate::storage::atomic::regular_file(&metadata) {
            return Err(Problem::new("导航页必须是普通文件。"));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            if metadata.uid() != rustix::process::getuid().as_raw()
                || metadata.permissions().mode() & 0o077 != 0
            {
                return Err(Problem::new("私有导航页必须属于当前用户，权限为 0600。"));
            }
        }
        read_page_data(page)?;
        return Ok(());
    }
    // The embedded template's machine snapshot is never reused on a new host.
    let data = json!({"updatedAt":super::stamp(),"host":host,"services":[]});
    super::write_private_bytes(page, replace_page_data(TEMPLATE, &data)?.as_bytes())
}

pub async fn serve(
    paths: AgentPaths,
    config_path: &Path,
    restart: watch::Sender<bool>,
    mut shutdown: watch::Receiver<bool>,
) -> io::Result<()> {
    let config = platform::load_config(config_path).map_err(io::Error::other)?;
    let host = platform::host_info(&config).map_err(io::Error::other)?;
    ensure_page(&config, &host).map_err(io::Error::other)?;
    let port = config["http"]["port"]
        .as_u64()
        .ok_or_else(|| io::Error::other("导航端口缺失"))? as u16;
    let bridge: Arc<dyn Backend> =
        Arc::new(platform::Bridge::new(config.clone(), paths, restart).map_err(io::Error::other)?);
    let manager = Manager::new(config.clone(), host.clone(), bridge).map_err(io::Error::other)?;
    let origins = ["127.0.0.1", "localhost", "[::1]"].map(|host| format!("http://{host}:{port}"));
    let state = Gateway {
        manager: Arc::new(Mutex::new(manager)),
        page: config["page"].as_str().unwrap().into(),
        host,
        token: Arc::new(zeroize::Zeroizing::new(format!("{}{}", id(), id()))),
        origins: Arc::new(origins.iter().cloned().collect()),
        hosts: Arc::new(
            origins
                .iter()
                .map(|s| s.trim_start_matches("http://").to_owned())
                .collect(),
        ),
        slots: Arc::new(Semaphore::new(8)),
    };
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    let app = router(state);
    let ipv6 = match tokio::net::TcpListener::bind(("::1", port)).await {
        Ok(listener) => Some(listener),
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::AddrNotAvailable | io::ErrorKind::Unsupported
            ) =>
        {
            None
        }
        Err(error) => return Err(error),
    };
    tracing::info!(port, "Veytrane 内置导航已启动");
    let mut servers = tokio::task::JoinSet::new();
    let child_app = app.clone();
    let mut child_stop = shutdown.clone();
    servers.spawn(async move {
        axum::serve(
            listener,
            child_app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(async move {
            let _ = child_stop.changed().await;
        })
        .await
    });
    if let Some(listener) = ipv6 {
        let mut child_stop = shutdown.clone();
        servers.spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .with_graceful_shutdown(async move {
                let _ = child_stop.changed().await;
            })
            .await
        });
    }
    tokio::select! {
        _ = shutdown.changed() => {
            while let Some(result) = servers.join_next().await {
                result.map_err(io::Error::other)??;
            }
            Ok(())
        }
        result = servers.join_next() => {
            match result {
                Some(result) => result.map_err(io::Error::other)?,
                None => Ok(()),
            }
        }
    }
}

fn router(state: Gateway) -> Router {
    Router::new()
        .route("/", get(page))
        .route("/index.html", get(page))
        .route("/service-navigation.html", get(page))
        .route("/api/navigation/bootstrap", get(bootstrap))
        .route("/api/navigation/{action}", post(api))
        .fallback(not_found)
        .layer(DefaultBodyLimit::max(32768))
        .layer(middleware::from_fn_with_state(state.clone(), boundary))
        .with_state(state)
}

fn error(problem: Problem) -> Response {
    let status = match problem.code.as_str() {
        "forbidden" => StatusCode::FORBIDDEN,
        "revision_conflict" | "id_conflict" | "port_in_use" => StatusCode::CONFLICT,
        _ if problem.uncertain => StatusCode::INTERNAL_SERVER_ERROR,
        _ => StatusCode::BAD_REQUEST,
    };
    (
        status,
        Json(json!({"error":{
            "message":problem.message,"code":problem.code,
            "uncertain":problem.uncertain,"details":problem.details
        }})),
    )
        .into_response()
}

fn same_token(left: &[u8], right: &[u8]) -> bool {
    let mut difference = left.len() ^ right.len();
    for (a, b) in left.iter().zip(right) {
        difference |= usize::from(a ^ b);
    }
    difference == 0
}

async fn boundary(State(state): State<Gateway>, request: Request, next: Next) -> Response {
    let headers = request.headers();
    let header = |name: &str| headers.get(name).and_then(|h| h.to_str().ok());
    let local = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .is_some_and(|address| address.0.ip().is_loopback());
    let path = request.uri().path();
    let static_page = matches!(path, "/" | "/index.html" | "/service-navigation.html");
    let allowed = local
        && header("host").is_some_and(|h| state.hosts.contains(h))
        && header("origin").is_none_or(|o| state.origins.contains(o))
        && matches!(
            header("sec-fetch-site"),
            None | Some("same-origin" | "none")
        );
    let auth = (static_page || header("x-navigation-client") == Some("1"))
        && (request.method() != Method::POST
            || (header("origin").is_some_and(|o| state.origins.contains(o))
                && same_token(
                    header("x-navigation-token").unwrap_or("").as_bytes(),
                    state.token.as_bytes(),
                )));
    let mut response = if !allowed || !auth {
        error(Problem::code(
            "管理接口仅允许本机导航页访问，请重新连接管理台。",
            "forbidden",
        ))
    } else if request.uri().query().is_some() {
        not_found().await
    } else if let Ok(_permit) = state.slots.clone().try_acquire_owned() {
        // Do not cancel a mutation on timeout: its durable receipt is authoritative.
        next.run(request).await
    } else {
        (
            StatusCode::TOO_MANY_REQUESTS,
            Json(json!({"error":{"message":"管理请求较多，请稍后重试。","code":"busy"}})),
        )
            .into_response()
    };
    for (name, value) in [
        ("cache-control", "no-store"),
        ("x-content-type-options", "nosniff"),
        ("x-frame-options", "DENY"),
        ("referrer-policy", "no-referrer"),
        (
            "content-security-policy",
            "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; connect-src 'self'; img-src 'self' data:; base-uri 'none'; frame-ancestors 'none'; form-action 'none'",
        ),
    ] {
        response.headers_mut().insert(
            HeaderName::from_static(name),
            HeaderValue::from_static(value),
        );
    }
    response
}

async fn page(State(state): State<Gateway>) -> Response {
    match tokio::fs::read_to_string(&state.page).await {
        Ok(html) => Html(html).into_response(),
        Err(_) => (StatusCode::SERVICE_UNAVAILABLE, "私有导航页面暂不可用").into_response(),
    }
}
async fn bootstrap(State(state): State<Gateway>) -> Response {
    Json(json!({"token":state.token.as_str(),"version":1,"host":state.host})).into_response()
}
async fn not_found() -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({"error":{"message":"接口不存在。","code":"not_found"}})),
    )
        .into_response()
}
async fn api(
    State(state): State<Gateway>,
    axum::extract::Path(action): axum::extract::Path<String>,
    body: std::result::Result<Json<Value>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Ok(Json(body)) = body else {
        return error(Problem::new("请求必须是有效的 JSON 对象。"));
    };
    if !body.is_object() {
        return error(Problem::new("请求必须是 JSON 对象。"));
    }
    let mut manager = state.manager.lock().await;
    let value = match action.as_str() {
        "state" => manager.state(body["refreshRemote"] == true).await,
        "catalog" => manager.catalog(body["peer"].as_str().unwrap_or("")).await,
        "preview" => manager.preview(&body).await,
        "diagnose" => manager.diagnose(body["rule"].as_str().unwrap_or("")).await,
        "apply" | "start" | "stop" | "restart_forward" | "reconnect" | "restart_agent" => {
            manager.mutate(&action, &body).await
        }
        _ => return not_found().await,
    };
    match value {
        Ok(value) => Json(json!({"result":value})).into_response(),
        Err(problem) => error(problem),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{Body, to_bytes},
        http::Request,
    };
    use tower::ServiceExt;

    struct ReadOnly;
    impl Backend for ReadOnly {
        fn call<'a>(
            &'a self,
            command: Value,
            remote: bool,
            _: Option<&'a str>,
        ) -> super::super::Call<'a> {
            Box::pin(async move {
                assert!(!remote, "HTTP tests must not use a real remote");
                match command["method"].as_str().unwrap() {
                    "config" => Ok(
                        json!({"revision":7,"settings":{"forwards":[],"service_ids":{},"services":[],"trusted_devices":[]}}),
                    ),
                    "status" => Ok(
                        json!({"core_running":true,"epoch":"fixture","node":{"node_id":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","peers":{}},"forwards":{"rules":[]}}),
                    ),
                    _ => panic!("HTTP read-only test attempted a mutation"),
                }
            })
        }
        fn target_probe<'a>(&'a self, _: &'a str) -> super::super::Call<'a> {
            Box::pin(async { panic!("unexpected target probe") })
        }
        fn restart<'a>(&'a self) -> super::super::Call<'a> {
            Box::pin(async { panic!("unexpected restart") })
        }
    }
    struct Fixture(std::path::PathBuf);
    impl Drop for Fixture {
        fn drop(&mut self) {
            // Exclusively created below; holds only mock test data.
            if self.0.parent() == Some(std::env::temp_dir().as_path())
                && self
                    .0
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("veytrane-native-navigation-")
            {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }
    fn fixture() -> (Fixture, Router) {
        let root = std::env::temp_dir().join(format!("veytrane-native-navigation-{}", id()));
        std::fs::create_dir(&root).unwrap();
        let host = json!({"platform":"linux","name":"Fixture","localAdminId":"local-admin","adminUrl":"http://127.0.0.1:7789/","agentRestartSupported":true});
        let page = root.join("navigation.html");
        let config = json!({"page":page,"state_dir":root.join("control"),"platform":"linux","local_name":"Fixture","remote":null,"peers":{}});
        ensure_page(&config, &host).unwrap();
        let manager = Manager::new(config, host.clone(), Arc::new(ReadOnly)).unwrap();
        let state = Gateway {
            manager: Arc::new(Mutex::new(manager)),
            page,
            host,
            token: Arc::new(zeroize::Zeroizing::new("fixture-secret".to_owned())),
            origins: Arc::new(HashSet::from(["http://127.0.0.1:3001".to_owned()])),
            hosts: Arc::new(HashSet::from(["127.0.0.1:3001".to_owned()])),
            slots: Arc::new(Semaphore::new(8)),
        };
        (Fixture(root), router(state))
    }
    fn request(method: Method, path: &str) -> axum::http::request::Builder {
        Request::builder()
            .method(method)
            .uri(path)
            .header("host", "127.0.0.1:3001")
    }
    fn connect(mut request: Request<Body>, address: &str) -> Request<Body> {
        request
            .extensions_mut()
            .insert(ConnectInfo(address.parse::<SocketAddr>().unwrap()));
        request
    }
    #[tokio::test]
    async fn navigation_page_and_legacy_urls_are_served_by_rust() {
        let (_fixture, app) = fixture();
        for path in ["/", "/index.html", "/service-navigation.html"] {
            let response = app
                .clone()
                .oneshot(connect(
                    request(Method::GET, path).body(Body::empty()).unwrap(),
                    "127.0.0.1:12345",
                ))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(response.headers()["cache-control"], "no-store");
            assert_eq!(response.headers()["x-frame-options"], "DENY");
            let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
            let html = std::str::from_utf8(&body).unwrap();
            assert!(html.contains("navigation-data"));
            let start = html.find(PAGE_START).unwrap() + PAGE_START.len();
            let data: Value =
                serde_json::from_str(html[start..].split(PAGE_END).next().unwrap()).unwrap();
            assert_eq!(data["host"]["name"], "Fixture");
            assert_eq!(
                data["services"],
                json!([]),
                "new hosts must not inherit the embedded Mac snapshot"
            );
        }
    }
    #[tokio::test]
    async fn navigation_rejects_cross_origin_host_and_nonlocal_clients() {
        let (_fixture, app) = fixture();
        for (host, origin, site, client) in [
            ("evil.test", "http://127.0.0.1:3001", "none", "127.0.0.1:1"),
            ("127.0.0.1:3001", "http://evil.test", "none", "127.0.0.1:1"),
            (
                "127.0.0.1:3001",
                "http://127.0.0.1:3001",
                "cross-site",
                "127.0.0.1:1",
            ),
            (
                "127.0.0.1:3001",
                "http://127.0.0.1:3001",
                "none",
                "192.0.2.1:1",
            ),
        ] {
            let req = Request::builder()
                .uri("/")
                .header("host", host)
                .header("origin", origin)
                .header("sec-fetch-site", site)
                .body(Body::empty())
                .unwrap();
            assert_eq!(
                app.clone()
                    .oneshot(connect(req, client))
                    .await
                    .unwrap()
                    .status(),
                StatusCode::FORBIDDEN
            );
        }
    }
    #[tokio::test]
    async fn navigation_bootstrap_requires_the_navigation_client_header() {
        let (_fixture, app) = fixture();
        let req = request(Method::GET, "/api/navigation/bootstrap")
            .body(Body::empty())
            .unwrap();
        assert_eq!(
            app.clone()
                .oneshot(connect(req, "127.0.0.1:1"))
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        let req = request(Method::GET, "/api/navigation/bootstrap")
            .header("x-navigation-client", "1")
            .body(Body::empty())
            .unwrap();
        let response = app.oneshot(connect(req, "127.0.0.1:1")).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let data: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 65536).await.unwrap()).unwrap();
        assert_eq!(data["host"]["name"], "Fixture");
        assert_eq!(data["token"], "fixture-secret");
    }
    #[tokio::test]
    async fn navigation_state_preserves_the_existing_frontend_contract() {
        let (_fixture, app) = fixture();
        let req = request(Method::POST, "/api/navigation/state")
            .header("x-navigation-client", "1")
            .header("origin", "http://127.0.0.1:3001")
            .header("x-navigation-token", "fixture-secret")
            .header("content-type", "application/json")
            .body(Body::from("{\"refreshRemote\":false}"))
            .unwrap();
        let response = app.oneshot(connect(req, "127.0.0.1:1")).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let data: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 65536).await.unwrap()).unwrap();
        assert_eq!(data["result"]["revision"], 7);
        assert_eq!(data["result"]["host"]["name"], "Fixture");
        assert_eq!(data["result"]["agent"]["running"], true);
    }
    #[tokio::test]
    async fn navigation_api_rejects_missing_token_and_never_serves_private_files() {
        let (_fixture, app) = fixture();
        let req = request(Method::POST, "/api/navigation/state")
            .header("x-navigation-client", "1")
            .header("origin", "http://127.0.0.1:3001")
            .header("content-type", "application/json")
            .body(Body::from("{}"))
            .unwrap();
        assert_eq!(
            app.clone()
                .oneshot(connect(req, "127.0.0.1:1"))
                .await
                .unwrap()
                .status(),
            StatusCode::FORBIDDEN
        );
        for path in [
            "/config.json",
            "/control/operations.json",
            "/../node/identity.key",
            "/?token=anything",
        ] {
            let req = request(Method::GET, path)
                .header("x-navigation-client", "1")
                .body(Body::empty())
                .unwrap();
            assert_eq!(
                app.clone()
                    .oneshot(connect(req, "127.0.0.1:1"))
                    .await
                    .unwrap()
                    .status(),
                StatusCode::NOT_FOUND
            );
        }
    }
}
