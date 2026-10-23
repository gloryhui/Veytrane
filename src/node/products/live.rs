//! WHIP publishing stays local. Only a read-only WHEP proxy and ICE/TCP media
//! are exported through the existing ID-scoped node TCP forwarding mechanism.
use super::sub2api::{authorized_peers, log_connection_event, stop_control};
use crate::node::{
    core::{
        config::{AllowedForwardTarget, TunnelRule},
        control,
        remote_auth::{RemoteVerifier, SecretPassword},
        settings::NodeSettings,
    },
    host::forwarding::ForwardManager,
};
use crate::{
    identity::{Identity, NodeId},
    instance_lock::InstanceLock,
};
use axum::{
    Router,
    body::{Body, to_bytes},
    extract::{Request, State},
    http::{HeaderValue, Method, StatusCode, header},
    response::{Html, IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    io,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub const VIEWER_URL: &str = "http://127.0.0.1:18889/";
const WEB_LISTEN: &str = "127.0.0.1:18889";
const PROXY_LISTEN: &str = "127.0.0.1:18890";
const MTX_HTTP: &str = "http://127.0.0.1:18891";
const MEDIA_LISTEN: &str = "127.0.0.1:18189";
const MEDIA_TARGET: &str = "127.0.0.1:18190";
const WEB_SERVICE: &str = "live-web";
const MEDIA_SERVICE: &str = "live-media";
const MAX_SDP: usize = 64 * 1024;
const PLAYER: &str = include_str!("../../live/player.html");
const PLAYER_JS: &str = include_str!("../../live/player.js");

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ViewerProfile {
    signal_server: String,
    server_node_id: String,
    password: String,
    #[serde(default)]
    signal_tls: Option<crate::discovery::signal_tls::SignalTlsClient>,
    #[serde(default)]
    relay_server: Option<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct GatewayProfile {
    signal_server: String,
    remote_auth: RemoteVerifier,
    #[serde(default)]
    signal_tls: Option<crate::discovery::signal_tls::SignalTlsClient>,
    #[serde(default)]
    relay_server: Option<String>,
}

fn error(message: impl ToString) -> io::Error {
    io::Error::other(message.to_string())
}

fn settings(root: PathBuf, signal: String) -> NodeSettings {
    let mut settings = NodeSettings::initial(root);
    settings.direct.enabled = false;
    settings.direct.lan_discovery = false;
    settings.signal_server = signal;
    settings.enable_files = false;
    settings.enable_speed = false;
    settings.auto_resume = false;
    settings
}

fn session(settings: &NodeSettings, loopback_test: bool) -> io::Result<control::NodeSessionConfig> {
    let mut config = settings.session().map_err(error)?;
    config.tunnel_rules.clear(); // The host ForwardManager owns all local listeners.
    if loopback_test {
        config.network.stun_servers.clear();
        config.network.include_loopback = true;
    }
    Ok(config)
}

fn services(peers: &BTreeSet<String>) -> Vec<AllowedForwardTarget> {
    [(WEB_SERVICE, PROXY_LISTEN), (MEDIA_SERVICE, MEDIA_TARGET)]
        .into_iter()
        .map(|(id, target)| AllowedForwardTarget {
            id: id.into(),
            name: id.into(),
            target: target.parse().unwrap(),
            allowed_peers: peers.iter().cloned().collect(),
            enabled: true,
        })
        .collect()
}

impl ViewerProfile {
    fn settings(&self, root: PathBuf) -> io::Result<NodeSettings> {
        let peer = NodeId::from_hex(&self.server_node_id).map_err(error)?;
        SecretPassword::new(self.password.clone()).map_err(error)?;
        let mut s = settings(root, self.signal_server.clone());
        // The viewer never grants the gateway access back into its computer.
        s.remote_auth = Some(
            RemoteVerifier::create(&SecretPassword::generate().map_err(error)?).map_err(error)?,
        );
        s.signal_tls = self.signal_tls.clone();
        s.relay_server = self.relay_server.clone();
        for (id, listen) in [(WEB_SERVICE, WEB_LISTEN), (MEDIA_SERVICE, MEDIA_LISTEN)] {
            s.forwards.push(TunnelRule {
                id: id.into(),
                name: id.into(),
                peer_node_id: peer.to_hex(),
                listen: listen.parse().unwrap(),
                target: "127.0.0.1:1".parse().unwrap(),
                enabled: true,
                auto_start: true,
            });
            s.service_ids.insert(id.into(), id.into());
        }
        s.validate().map_err(error)?;
        Ok(s)
    }
}

pub fn initialize_gateway(root: &Path, signal: String, relay: Option<String>) -> io::Result<()> {
    crate::storage::permissions::ensure_directory(root)?;
    let _lock = InstanceLock::acquire(&root.join("gateway.lock")).map_err(error)?;
    if ["gateway.json", "identity.key", "live-viewer.json"]
        .iter()
        .any(|name| root.join(name).exists())
    {
        return Err(error("直播网关已初始化；禁止覆盖身份或观看配置"));
    }
    let password = SecretPassword::generate().map_err(error)?;
    let verifier = RemoteVerifier::create(&password).map_err(error)?;
    let mut checked = settings(root.to_owned(), signal.clone());
    checked.relay_server = relay.clone();
    checked.remote_auth = Some(verifier.clone());
    checked.validate().map_err(error)?;
    let identity = Identity::generate();
    // SecretPassword formats as redacted; use its explicit exposure API only in
    // the operator-owned viewer profile. Never print it or put it in HTML.
    let viewer = ViewerProfile {
        signal_server: signal.clone(),
        server_node_id: identity.node_id().to_hex(),
        password: password.expose().to_owned(),
        signal_tls: None,
        relay_server: relay.clone(),
    };
    viewer.settings(root.to_owned())?;
    let gateway = GatewayProfile {
        signal_server: signal,
        remote_auth: verifier,
        signal_tls: None,
        relay_server: relay,
    };
    identity.save(&root.join("identity.key")).map_err(error)?;
    write_private(
        &root.join("gateway.json"),
        &serde_json::to_vec_pretty(&gateway).map_err(error)?,
    )?;
    write_private(
        &root.join("live-viewer.json"),
        &serde_json::to_vec_pretty(&viewer).map_err(error)?,
    )?;
    Ok(())
}

fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)
}

/// Keep the gateway's local UI behind the service allowlist. MediaMTX's WHIP
/// publishing route is deliberately absent from this router.
fn router(client: reqwest::Client) -> Router {
    Router::new().fallback(read_only_http).with_state(client)
}

fn whep_path(method: &Method, path: &str) -> bool {
    match (method, path) {
        (&Method::POST | &Method::OPTIONS, "/live/whep") => true,
        (&Method::PATCH | &Method::DELETE | &Method::OPTIONS, _) => {
            path.strip_prefix("/live/whep/").is_some_and(|id| {
                !id.is_empty()
                    && id.len() <= 128
                    && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            })
        }
        _ => false,
    }
}

fn tcp_answer(sdp: &str) -> Result<String, &'static str> {
    let mut lines = Vec::new();
    let mut found = false;
    for line in sdp.lines() {
        if let Some(candidate) = line.strip_prefix("a=candidate:") {
            let mut fields: Vec<_> = candidate.split_ascii_whitespace().collect();
            // Restrict browser connections to the viewer's one local TCP port.
            if fields.len() < 8
                || !fields[2].eq_ignore_ascii_case("tcp")
                || fields[4] != "127.0.0.1"
                || fields[5] != "18190"
            {
                continue;
            }
            fields[5] = "18189";
            lines.push(format!("a=candidate:{}", fields.join(" ")));
            found = true;
        } else {
            lines.push(line.to_owned());
        }
    }
    if !found {
        return Err("MediaMTX 未提供本机 TCP 媒体候选地址，请使用随附配置");
    }
    Ok(lines.join("\r\n") + "\r\n")
}

async fn read_only_http(State(client): State<reqwest::Client>, request: Request) -> Response {
    let path = request.uri().path();
    if request.method() == Method::GET {
        return match path {
            "/" => Html(PLAYER).into_response(),
            "/player.js" => (
                [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                PLAYER_JS,
            )
                .into_response(),
            "/healthz" => "Veytrane Live".into_response(),
            _ => StatusCode::NOT_FOUND.into_response(),
        };
    }
    if request.uri().query().is_some() || !whep_path(request.method(), path) {
        return StatusCode::NOT_FOUND.into_response();
    }
    // Do not forward user-controlled Host, Authorization, cookies, connection
    // headers or destinations. All upstream requests go to this fixed endpoint.
    let mut upstream = client.request(request.method().clone(), format!("{MTX_HTTP}{path}"));
    for name in [header::CONTENT_TYPE, header::IF_MATCH] {
        if let Some(value) = request.headers().get(&name) {
            upstream = upstream.header(name, value);
        }
    }
    let body = match tokio::time::timeout(
        Duration::from_secs(5),
        to_bytes(request.into_body(), MAX_SDP),
    )
    .await
    {
        Ok(Ok(body)) => body,
        _ => return StatusCode::PAYLOAD_TOO_LARGE.into_response(),
    };
    let result = upstream.body(body).send().await;
    let mut upstream = match result {
        Ok(response) => response,
        Err(_) => return (StatusCode::BAD_GATEWAY, "等待主播启动 MediaMTX").into_response(),
    };
    let mut response = Response::builder().status(upstream.status());
    for name in [
        header::CONTENT_TYPE,
        header::ETAG,
        axum::http::HeaderName::from_static("accept-patch"),
    ] {
        if let Some(value) = upstream.headers().get(&name) {
            response = response.header(name, value);
        }
    }
    if let Some(location) = upstream.headers().get(header::LOCATION) {
        let Some(path) = location
            .to_str()
            .ok()
            .and_then(|v| v.strip_prefix(MTX_HTTP).or(Some(v)))
        else {
            return StatusCode::BAD_GATEWAY.into_response();
        };
        if !whep_path(&Method::DELETE, path) {
            return StatusCode::BAD_GATEWAY.into_response();
        }
        response = response.header(header::LOCATION, path);
    }
    let status = upstream.status();
    let is_sdp = upstream
        .headers()
        .get(header::CONTENT_TYPE)
        .is_some_and(|v| v.as_bytes().starts_with(b"application/sdp"));
    let bytes = tokio::time::timeout(Duration::from_secs(8), async {
        let mut bytes = Vec::new();
        while let Some(chunk) = upstream.chunk().await.map_err(error)? {
            if bytes.len() + chunk.len() > MAX_SDP {
                return Err(error("上游响应过大"));
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok::<_, io::Error>(bytes)
    })
    .await;
    let bytes = match bytes {
        Ok(Ok(bytes)) => bytes,
        _ => return StatusCode::BAD_GATEWAY.into_response(),
    };
    let body = if status.is_success() && is_sdp {
        match std::str::from_utf8(&bytes)
            .ok()
            .and_then(|s| tcp_answer(s).ok())
        {
            Some(answer) => Body::from(answer),
            None => return (StatusCode::BAD_GATEWAY, "MediaMTX TCP 配置不匹配").into_response(),
        }
    } else {
        Body::from(bytes)
    };
    let mut result = response.body(body).unwrap();
    result
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    result
}

pub async fn run_gateway(root: &Path, loopback_test: bool) -> io::Result<()> {
    crate::storage::permissions::ensure_directory(root)?;
    let _lock = InstanceLock::acquire(&root.join("gateway.lock")).map_err(error)?;
    let identity = Identity::load(&root.join("identity.key")).map_err(error)?;
    let profile: GatewayProfile =
        serde_json::from_slice(&std::fs::read(root.join("gateway.json"))?)
            .map_err(|_| error("直播网关配置无效"))?;
    let mut s = settings(root.to_owned(), profile.signal_server);
    s.remote_auth = Some(profile.remote_auth);
    s.signal_tls = profile.signal_tls;
    s.relay_server = profile.relay_server;
    s.services = services(&BTreeSet::new());
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(error)?;
    let listener = tokio::net::TcpListener::bind(PROXY_LISTEN).await?;
    let mut web = tokio::spawn(async move { axum::serve(listener, router(client)).await });
    println!(
        "Veytrane Live 主播网关\nOBS WHIP：http://127.0.0.1:18891/live/whip\n仅发布观看网页和媒体通道；不录制、不提供管理接口。"
    );
    let result = gateway_loop(identity, s, loopback_test, &mut web).await;
    if !web.is_finished() {
        web.abort();
        let _ = web.await;
    }
    result
}

async fn gateway_loop(
    identity: Identity,
    s: NodeSettings,
    loopback_test: bool,
    web: &mut tokio::task::JoinHandle<io::Result<()>>,
) -> io::Result<()> {
    let shutdown = tokio::signal::ctrl_c();
    tokio::pin!(shutdown);
    loop {
        let (control, mut events) = control::spawn(identity.clone(), session(&s, loopback_test)?)?;
        let mut allowed = BTreeSet::new();
        let mut policy = super::recovery::GatewayRecovery::default();
        let started = Instant::now();
        let mut interval = tokio::time::interval(Duration::from_millis(250));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let recovery = crate::node::host::recovery::wait(control.clone());
        tokio::pin!(recovery);
        let reason = loop {
            tokio::select! {
                _ = &mut shutdown => break None,
                result = &mut *web => {
                    stop_control(control).await;
                    return Err(error(format!("直播网页服务已退出：{result:?}")));
                },
                reason = &mut recovery => break Some(reason.to_string()),
                event = events.recv() => match event {
                    Some(event) => { policy.event(started.elapsed(), &event); log_connection_event(&event); },
                    None => break Some("网络核心已退出".into()),
                },
                _ = interval.tick() => {
                    let snapshot = match control.snapshot() { Ok(s) => s, Err(e) => break Some(e.to_string()) };
                    if let Some(reason) = policy.reason(started.elapsed(), &snapshot) { break Some(reason.into()); }
                    let next = authorized_peers(snapshot);
                    if next != allowed {
                        if let Err(e) = control.update_tunnel_settings(services(&next), vec![]) { break Some(e); }
                        println!("已连接观看设备：{}", next.len());
                        allowed = next;
                    }
                }
            }
        };
        stop_control(control).await;
        let Some(reason) = reason else { return Ok(()) };
        println!("正在恢复直播连接：{reason}");
        tokio::select! { _ = &mut shutdown => return Ok(()), _ = tokio::time::sleep(Duration::from_secs(2)) => {} }
    }
}

pub async fn run_viewer(
    profile: &Path,
    root: Option<PathBuf>,
    browser: bool,
    loopback_test: bool,
) -> io::Result<()> {
    let profile: ViewerProfile = serde_json::from_slice(&std::fs::read(profile)?)
        .map_err(|_| error("观看配置无效；请使用主播提供的 live-viewer.json"))?;
    let root = match root {
        Some(root) => root,
        None => dirs::data_local_dir()
            .ok_or_else(|| error("无法找到用户目录"))?
            .join("Veytrane-Live-Viewer"),
    };
    crate::storage::permissions::ensure_directory(&root)?;
    let _lock = InstanceLock::acquire(&root.join("viewer.lock"))
        .map_err(|_| error("观看程序已启动或身份目录不可用"))?;
    let s = profile.settings(root.clone())?;
    let identity = Identity::load_or_create(&root.join("identity.key")).map_err(error)?;
    let peer = NodeId::from_hex(&profile.server_node_id).map_err(error)?;
    let password = SecretPassword::new(profile.password).map_err(error)?;
    let shutdown = tokio::signal::ctrl_c();
    tokio::pin!(shutdown);
    let mut opened = false;
    println!("Veytrane Live 观看端\n观看地址：{VIEWER_URL}\n保持此窗口运行，Ctrl+C 退出。");
    loop {
        let (control, mut events) = control::spawn(identity.clone(), session(&s, loopback_test)?)?;
        let mut forwards = ForwardManager::default();
        let prepared = match forwards.prepare(&s, &control, None).await {
            Ok(plan) => plan,
            Err(e) => {
                stop_control(control).await;
                return Err(error(format!("观看端口 18889 或 18189 不可用：{e}")));
            }
        };
        let _ = forwards.commit(prepared);
        let updates = match control.connection_updates(peer).await {
            Ok(updates) => updates,
            Err(e) => {
                forwards.shutdown().await;
                stop_control(control).await;
                return Err(error(e));
            }
        };
        if let Err(e) = control.connect_peer_with_password(peer, password.clone()) {
            forwards.shutdown().await;
            stop_control(control).await;
            return Err(error(e));
        }
        let mut interval = tokio::time::interval(Duration::from_secs(2));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let recovery = crate::node::host::recovery::wait(control.clone());
        tokio::pin!(recovery);
        let started = Instant::now();
        let mut watchdog = super::recovery::ClientRecovery::default();
        let mut directory_tick = Instant::now() - Duration::from_secs(15);
        let mut was_ready = false;
        let restart = loop {
            tokio::select! {
                _ = &mut shutdown => break false,
                _ = &mut recovery => break true,
                event = events.recv() => match event { Some(event) => log_connection_event(&event), None => break true },
                _ = interval.tick() => {
                    let connected = updates.borrow().as_ref().is_some_and(|c| c.close_reason().is_none());
                    if !connected { let _ = control.request_peer_connection(peer); }
                    let ready = connected && http_ready().await;
                    let directory = if connected && !ready && directory_tick.elapsed() >= Duration::from_secs(15) {
                        directory_tick = Instant::now();
                        let result = tokio::time::timeout(Duration::from_secs(3), control.list_services(peer)).await;
                        Some(matches!(result, Ok(Ok(ref entries)) if [WEB_SERVICE, MEDIA_SERVICE].iter().all(|id| entries.iter().any(|e| e.id == *id))))
                    } else { None };
                    if let Some(reason) = watchdog.sample(started.elapsed(), connected, ready, directory) {
                        println!("正在重建观看连接：{reason}"); break true;
                    }
                    if ready && !was_ready { println!("观看通道已就绪：{VIEWER_URL}"); }
                    if !ready && was_ready { println!("连接中断，正在自动恢复…"); }
                    was_ready = ready;
                    if ready && !opened {
                        if browser && open_browser().is_err() { println!("请手动打开：{VIEWER_URL}"); }
                        opened = true;
                    }
                }
            }
        };
        forwards.shutdown().await;
        stop_control(control).await;
        if !restart {
            return Ok(());
        }
        tokio::select! { _ = &mut shutdown => return Ok(()), _ = tokio::time::sleep(Duration::from_secs(2)) => {} }
    }
}

async fn http_ready() -> bool {
    tokio::time::timeout(Duration::from_secs(3), async {
        let mut stream = tokio::net::TcpStream::connect(WEB_LISTEN).await?;
        stream
            .write_all(b"GET /healthz HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
            .await?;
        let mut bytes = [0; 12];
        stream.read_exact(&mut bytes).await?;
        Ok::<_, io::Error>(&bytes == b"HTTP/1.1 200")
    })
    .await
    .is_ok_and(|r| r.unwrap_or(false))
}

fn open_browser() -> io::Result<()> {
    #[cfg(windows)]
    let result = std::process::Command::new("cmd")
        .args(["/D", "/C", "start", "", VIEWER_URL])
        .status();
    #[cfg(target_os = "macos")]
    let result = std::process::Command::new("open").arg(VIEWER_URL).status();
    #[cfg(not(any(windows, target_os = "macos")))]
    let result = std::process::Command::new("xdg-open")
        .arg(VIEWER_URL)
        .status();
    result.and_then(|s| {
        if s.success() {
            Ok(())
        } else {
            Err(error("无法打开浏览器"))
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tower::ServiceExt;

    #[test]
    fn media_candidates_only_point_to_the_viewers_tcp_forward() {
        let sdp = "v=0\r\na=candidate:1 1 udp 1 127.0.0.1 18190 typ host\r\na=candidate:2 1 tcp 1 127.0.0.1 18190 typ host tcptype passive\r\na=candidate:3 1 tcp 1 192.168.1.2 18190 typ host\r\na=end-of-candidates\r\n";
        let answer = tcp_answer(sdp).unwrap();
        assert!(answer.contains("tcp 1 127.0.0.1 18189"));
        assert!(
            !answer.contains("udp") && !answer.contains("192.168") && !answer.contains("18190")
        );
        assert!(tcp_answer("v=0\r\n").is_err());
    }

    #[tokio::test]
    async fn viewers_cannot_publish_or_reach_management() {
        let app = router(reqwest::Client::builder().no_proxy().build().unwrap());
        for (method, path) in [
            ("POST", "/live/whip"),
            ("POST", "/other/whep"),
            ("GET", "/v3/paths/list"),
            ("DELETE", "/live/whep/../whip"),
            ("POST", "/live/whep?path=other"),
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri(path)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{method} {path}");
        }
        assert_eq!(
            app.oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
    }

    #[test]
    fn viewer_has_only_two_fixed_forwards_and_no_inbound_services() {
        let profile = ViewerProfile {
            signal_server: "127.0.0.1:8900".into(),
            server_node_id: Identity::generate().node_id().to_hex(),
            password: "a7B8c9D0e1F2".into(),
            signal_tls: None,
            relay_server: None,
        };
        let s = profile.settings(std::env::temp_dir()).unwrap();
        assert_eq!(s.forwards.len(), 2);
        assert!(s.services.is_empty() && !s.enable_files && !s.enable_speed);
        assert_eq!(
            services(&BTreeSet::new())
                .iter()
                .map(|s| s.id.as_str())
                .collect::<Vec<_>>(),
            [WEB_SERVICE, MEDIA_SERVICE]
        );
    }

    #[test]
    fn setup_preserves_existing_identities() {
        let root = std::env::temp_dir().join(format!("live-init-{:032x}", rand::random::<u128>()));
        initialize_gateway(&root, "127.0.0.1:8900".into(), None).unwrap();
        let before = std::fs::read(root.join("identity.key")).unwrap();
        assert!(initialize_gateway(&root, "127.0.0.1:8900".into(), None).is_err());
        assert_eq!(before, std::fs::read(root.join("identity.key")).unwrap());
        let viewer: ViewerProfile =
            serde_json::from_slice(&std::fs::read(root.join("live-viewer.json")).unwrap()).unwrap();
        assert_eq!(
            viewer.server_node_id,
            Identity::load(&root.join("identity.key"))
                .unwrap()
                .node_id()
                .to_hex()
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
