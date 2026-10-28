//! Distribution mode: per-device identities and a shared, service-scoped secret.
//! Neither process starts the general Agent, file store, Web UI or IPC server.
use crate::node::core::config::{AllowedForwardTarget, TunnelRule};
use crate::node::core::control::SessionEvent;
use crate::node::core::control::{self, NodeControl, NodeSnapshot};
use crate::node::core::network_state::{NetworkLifecycle, PeerLifecycle};
use crate::node::core::remote_auth::{RemoteVerifier, SecretPassword};
use crate::node::core::settings::NodeSettings;
use crate::node::host::forwarding::ForwardManager;
use crate::storage::permissions as private_access;
use crate::{
    identity::{Identity, NodeId},
    instance_lock::InstanceLock,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    io,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub const LOCAL_URL: &str = "http://127.0.0.1:18081/";
const LISTEN: &str = "127.0.0.1:18081";
const TARGET: &str = "127.0.0.1:8080";
const SERVICE: &str = "sub2api";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ClientProfile {
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
    signal_tls: Option<crate::discovery::signal_tls::SignalTlsClient>,
    relay_server: Option<String>,
}

fn error(message: impl ToString) -> io::Error {
    io::Error::other(message.to_string())
}

fn fixed_settings(root: PathBuf, signal: String) -> NodeSettings {
    let mut settings = NodeSettings::initial(root);
    settings.direct.enabled = false;
    settings.direct.lan_discovery = false;
    settings.signal_server = signal;
    settings.enable_files = false;
    settings.enable_speed = false;
    settings.enable_tcp = true;
    settings.auto_resume = false;
    settings
}

impl ClientProfile {
    fn settings(&self, root: PathBuf) -> io::Result<NodeSettings> {
        let peer = NodeId::from_hex(&self.server_node_id).map_err(error)?;
        SecretPassword::new(self.password.clone()).map_err(error)?;
        let mut settings = fixed_settings(root, self.signal_server.clone());
        // The symmetric handshake requires a local challenge as well. Its
        // random secret is never shared, so it grants the gateway no incoming
        // rights on the recipient's computer and is not a second user setting.
        settings.remote_auth = Some(
            RemoteVerifier::create(&SecretPassword::generate().map_err(error)?).map_err(error)?,
        );
        settings.signal_tls = self.signal_tls.clone();
        settings.relay_server = self.relay_server.clone();
        settings.forwards = vec![TunnelRule {
            id: SERVICE.into(),
            name: "Sub2API".into(),
            peer_node_id: peer.to_hex(),
            listen: LISTEN.parse().unwrap(),
            // The node protocol uses service_ids, never a client-selected remote target.
            target: "127.0.0.1:1".parse().unwrap(),
            enabled: true,
            auto_start: true,
        }];
        settings.service_ids.insert(SERVICE.into(), SERVICE.into());
        settings.validate().map_err(error)?;
        Ok(settings)
    }
}

fn authorized_peers(snapshot: NodeSnapshot) -> BTreeSet<String> {
    snapshot
        .peers
        .into_values()
        .filter(|p| p.phase == "connected" && p.inbound_password)
        .map(|p| p.node_id)
        .collect()
}

fn only_service(peers: &BTreeSet<String>) -> AllowedForwardTarget {
    AllowedForwardTarget {
        id: SERVICE.into(),
        name: "Sub2API".into(),
        target: TARGET.parse().unwrap(),
        allowed_peers: peers.iter().cloned().collect(),
        enabled: true,
    }
}

/// Log connection stages, never profiles, passwords, proof material or tokens.
fn log_connection_event(event: &SessionEvent) {
    match event {
        SessionEvent::Lifecycle(NetworkLifecycle::ConnectingSignal) => {
            println!("正在连接信令服务器…")
        }
        SessionEvent::Lifecycle(NetworkLifecycle::SignalOnline) => {
            println!("信令服务器已连接，设备已登记。")
        }
        SessionEvent::Lifecycle(NetworkLifecycle::ReconnectingSignal { attempt, delay }) => {
            println!(
                "信令连接中断，第 {attempt} 次重试，等待 {} 秒。",
                delay.as_secs()
            )
        }
        SessionEvent::Lifecycle(NetworkLifecycle::Failed { detail }) => {
            println!("网络启动失败：{detail}")
        }
        SessionEvent::NetworkFamilies {
            ipv4,
            ipv6,
            relay_configured,
        } => println!("网络能力：IPv4={ipv4}，IPv6={ipv6}，中继兜底={relay_configured}"),
        SessionEvent::PeerCandidates {
            peer, ipv4, ipv6, ..
        } => println!(
            "设备 {}：已收到信令地址，IPv4={ipv4}，IPv6={ipv6}",
            peer.short()
        ),
        SessionEvent::PeerTransport {
            peer,
            relay,
            family,
            ..
        } => println!(
            "设备 {}：已建立 {} {family} 通道，正在校验身份。",
            peer.short(),
            if *relay { "中继" } else { "直连" },
        ),
        SessionEvent::PeerState { peer, state, .. } => {
            let detail = match state {
                PeerLifecycle::PeerPending => "等待信令配对".to_owned(),
                PeerLifecycle::Punching => "正在 UDP 打洞".to_owned(),
                PeerLifecycle::Authenticating => "正在建立加密通道并校验设备身份".to_owned(),
                PeerLifecycle::Negotiating => "正在协商服务协议".to_owned(),
                PeerLifecycle::RemoteAuthPending => "正在验证专用访问凭证".to_owned(),
                PeerLifecycle::Connected(_) => "设备通道与访问凭证认证完成".to_owned(),
                PeerLifecycle::Disconnected => "设备通道已断开，等待重新连接".to_owned(),
                PeerLifecycle::Failed(reason) => format!("连接失败：{reason}"),
            };
            println!("设备 {}：{detail}", peer.short());
        }
        SessionEvent::Diagnostic(message) => println!("连接诊断：{message}"),
        _ => {}
    }
}

/// Admin-only provisioning. Existing identities/configurations are never replaced.
pub fn initialize_gateway(root: &Path, signal: String, password: String) -> io::Result<NodeId> {
    private_access::ensure_directory(root)?;
    let _lock = InstanceLock::acquire(&root.join("gateway.lock")).map_err(error)?;
    if root.join("gateway.json").exists() || root.join("identity.key").exists() {
        return Err(error("专用网关已初始化；禁止覆盖其身份或访问凭证"));
    }
    let secret = SecretPassword::new(password).map_err(error)?;
    let profile = GatewayProfile {
        signal_server: signal,
        remote_auth: RemoteVerifier::create(&secret).map_err(error)?,
        signal_tls: None,
        relay_server: None,
    };
    let mut settings = fixed_settings(root.to_owned(), profile.signal_server.clone());
    settings.remote_auth = Some(profile.remote_auth.clone());
    settings.validate().map_err(error)?;
    let identity = Identity::generate();
    identity.save(&root.join("identity.key")).map_err(error)?;
    // The enclosing directory is private; no cleartext password is saved here.
    std::fs::write(
        root.join("gateway.json"),
        serde_json::to_vec_pretty(&profile).map_err(error)?,
    )?;
    Ok(identity.node_id())
}

pub async fn run_gateway(root: &Path) -> io::Result<()> {
    private_access::ensure_directory(root)?;
    let _lock = InstanceLock::acquire(&root.join("gateway.lock")).map_err(error)?;
    let identity = Identity::load(&root.join("identity.key")).map_err(error)?;
    let profile: GatewayProfile =
        serde_json::from_slice(&std::fs::read(root.join("gateway.json"))?)
            .map_err(|_| error("专用网关配置无效"))?;
    let mut settings = fixed_settings(root.to_owned(), profile.signal_server);
    settings.remote_auth = Some(profile.remote_auth);
    settings.signal_tls = profile.signal_tls;
    settings.relay_server = profile.relay_server;
    settings.services = vec![only_service(&BTreeSet::new())];
    println!("Veytrane Gateway for Sub2API started; files/speed/management disabled");
    let shutdown = tokio::signal::ctrl_c();
    tokio::pin!(shutdown);
    let mut generation = 0u64;
    let mut last_recovery: Option<String> = None;
    loop {
        generation = generation.saturating_add(1);
        let started = Instant::now();
        let (control, mut events) =
            control::spawn(identity.clone(), settings.session().map_err(error)?)?;
        let mut allowed = BTreeSet::new();
        let mut policy = crate::node::products::recovery::GatewayRecovery::default();
        let mut interval = tokio::time::interval(Duration::from_millis(250));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let host_recovery = crate::node::host::recovery::wait(control.clone());
        tokio::pin!(host_recovery);
        let mut status_tick = Instant::now() - Duration::from_secs(2);
        println!("专用网关网络代次 {generation}：重新收集网络地址并登记信令。");
        let refresh = loop {
            tokio::select! {
                _ = &mut shutdown => break None,
                reason = &mut host_recovery => break Some(reason.to_string()),
                event = events.recv() => match event {
                    Some(event) => {policy.event(started.elapsed(), &event);log_connection_event(&event);},
                    None => break Some("网络核心已退出，自动重新建立连接".into()),
                },
                _ = interval.tick() => {
                    let snapshot = match control.snapshot() {Ok(s)=>s, Err(e)=>break Some(e.to_string())};
                    if status_tick.elapsed() >= Duration::from_secs(2) {
                        // Private, non-authoritative diagnostics; never a management API.
                        let status=serde_json::json!({"core_generation":generation,"node_id":identity.node_id().to_hex(),"signal":snapshot.signal,"peers":snapshot.peers,"last_recovery":last_recovery});
                        let pending=root.join("runtime.pending.json");
                        if std::fs::write(&pending,status.to_string()).and_then(|()|std::fs::rename(&pending,root.join("runtime.json"))).is_err() {
                            eprintln!("无法更新网关运行状态文件；业务连接继续运行");
                        }
                        status_tick=Instant::now();
                    }
                    if let Some(reason)=policy.reason(started.elapsed(),&snapshot) {break Some(reason.into());}
                    let next = authorized_peers(snapshot);
                    if next != allowed {
                        if let Err(e)=control.update_tunnel_settings(vec![only_service(&next)], vec![]) {break Some(e);}
                        println!("Password-authorized devices: {}", next.len());
                        allowed = next;
                    }
                }
            }
        };
        stop_control(control).await;
        let Some(reason) = refresh else {
            return Ok(());
        };
        println!("自动恢复：{reason}。身份和访问凭证保持不变。");
        last_recovery = Some(reason);
        tokio::select! {
            _ = &mut shutdown => return Ok(()),
            _ = tokio::time::sleep(Duration::from_secs(2)) => {}
        }
    }
}

async fn stop_control(control: NodeControl) {
    control.shutdown();
    let _ = tokio::task::spawn_blocking(move || control.shutdown_and_wait()).await;
}

pub async fn run_client(embedded_profile: &str) -> io::Result<()> {
    let profile: ClientProfile = serde_json::from_str(embedded_profile)
        .map_err(|_| error("此构建没有有效的专用访问凭证，请使用正式分发包"))?;
    let root = dirs::data_local_dir()
        .ok_or_else(|| error("无法找到当前用户的数据目录"))?
        .join("Sub2API-Client");
    private_access::ensure_directory(&root)?;
    let _lock = InstanceLock::acquire(&root.join("client.lock"))
        .map_err(|_| error("程序已经启动，或无法访问本机数据目录"))?;
    let settings = profile.settings(root.clone())?;
    let identity = Identity::load_or_create(&root.join("identity.key")).map_err(error)?;
    println!(
        "Veytrane for Sub2API\n本地地址：{LOCAL_URL}\n设备身份：{}\n保持此窗口运行；按 Ctrl+C 停止。",
        identity.node_id()
    );
    let peer = NodeId::from_hex(&profile.server_node_id).map_err(error)?;
    let password = SecretPassword::new(profile.password.clone()).map_err(error)?;
    let shutdown = tokio::signal::ctrl_c();
    tokio::pin!(shutdown);
    let mut opened = false;
    loop {
        let mut session = settings.session().map_err(error)?;
        session.tunnel_rules.clear();
        let (control, mut events) = control::spawn(identity.clone(), session)?;
        let mut forwards = ForwardManager::default();
        let prepared = match forwards.prepare(&settings, &control, None).await {
            Ok(plan) => plan,
            Err(_) => {
                stop_control(control).await;
                return Err(error("本机端口 18081 已被占用。请关闭占用程序后重试。"));
            }
        };
        let _ = forwards.commit(prepared);
        let connection_updates = control.connection_updates(peer).await.map_err(error)?;
        control
            .connect_peer_with_password(peer, password.clone())
            .map_err(error)?;
        let recovery = crate::node::host::recovery::wait(control.clone());
        tokio::pin!(recovery);
        let mut interval = tokio::time::interval(Duration::from_secs(3));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut ready_before = false;
        let started = Instant::now();
        let mut watchdog = crate::node::products::recovery::ClientRecovery::default();
        let mut directory_tick = Instant::now() - Duration::from_secs(15);
        println!("正在连接；网络恢复后会自动重试…");
        let restart = loop {
            tokio::select! {
                _ = &mut shutdown => break false,
                _ = &mut recovery => break true,
                event = events.recv() => match event {
                    Some(event) => log_connection_event(&event),
                    None => break true,
                },
                _ = interval.tick() => {
                    // Healthy QUIC sessions do not need a new signaling pairing
                    // every readiness poll. Retry only when the peer is absent.
                    let peer_ready = connection_updates.borrow().as_ref().is_some_and(|c| c.close_reason().is_none());
                    if !peer_ready {
                        let _ = control.request_peer_connection(peer);
                    }
                    let ready = peer_ready && http_ready().await;
                    let directory = if peer_ready && !ready && directory_tick.elapsed() >= Duration::from_secs(15) {
                        directory_tick=Instant::now();
                        let result=tokio::time::timeout(Duration::from_secs(3),control.list_services(peer)).await;
                        let accessible=matches!(result,Ok(Ok(ref entries)) if entries.iter().any(|s|s.id==SERVICE));
                        if accessible {println!("设备通道正常，等待 Ubuntu 上的 Sub2API 服务响应…");}
                        Some(accessible)
                    } else {None};
                    if let Some(reason)=watchdog.sample(started.elapsed(),peer_ready,ready,directory) {
                        println!("自动恢复：{reason}。");
                        break true;
                    }
                    if ready && !ready_before {
                        println!("Sub2API 已连接：{LOCAL_URL}");
                    } else if !ready && ready_before {
                        println!("连接中断，正在自动重连…");
                    }
                    ready_before = ready;
                    if ready && !opened {
                        if open_browser().is_err() { println!("请在浏览器打开：{LOCAL_URL}"); }
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
        println!("正在重建网络并重新连接，设备身份保持不变…");
        tokio::select! {
            _ = &mut shutdown => return Ok(()),
            _ = tokio::time::sleep(Duration::from_secs(2)) => {}
        }
    }
}

async fn http_ready() -> bool {
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut tcp = tokio::net::TcpStream::connect(LISTEN).await?;
        tcp.write_all(b"GET / HTTP/1.1\r\nHost: 127.0.0.1:18081\r\nConnection: close\r\n\r\n")
            .await?;
        let mut bytes = [0; 12];
        tcp.read_exact(&mut bytes).await?;
        Ok::<_, io::Error>(bytes.starts_with(b"HTTP/1."))
    })
    .await
    .is_ok_and(|r| r.unwrap_or(false))
}

fn open_browser() -> io::Result<()> {
    #[cfg(windows)]
    let result = std::process::Command::new("cmd")
        .args(["/D", "/C", "start", "", LOCAL_URL])
        .status();
    #[cfg(target_os = "macos")]
    let result = std::process::Command::new("open").arg(LOCAL_URL).status();
    #[cfg(not(any(windows, target_os = "macos")))]
    let result = std::process::Command::new("xdg-open")
        .arg(LOCAL_URL)
        .status();
    result.and_then(|s| {
        if s.success() {
            Ok(())
        } else {
            Err(error("browser launch failed"))
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::core::control::PeerSnapshot;

    #[test]
    fn profile_cannot_select_ports_targets_or_management_features() {
        let server = Identity::generate().node_id().to_hex();
        let profile = serde_json::json!({"signal_server":"127.0.0.1:8900", "server_node_id":server, "password":"a7B8c9D0e1F2"});
        let parsed: ClientProfile = serde_json::from_value(profile.clone()).unwrap();
        let settings = parsed.settings(std::env::temp_dir()).unwrap();
        assert!(!settings.enable_files && !settings.enable_speed);
        assert!(settings.services.is_empty() && settings.trusted_devices.is_empty());
        assert!(!settings.direct.enabled && !settings.direct.lan_discovery);
        assert_eq!(settings.forwards.len(), 1);
        assert_eq!(settings.forwards[0].listen.to_string(), LISTEN);
        assert_eq!(settings.service_ids[&settings.forwards[0].id], SERVICE);
        for key in [
            "listen",
            "target",
            "enable_files",
            "services",
            "admin_token",
        ] {
            let mut altered = profile.clone();
            altered[key] = serde_json::json!("override");
            assert!(serde_json::from_value::<ClientProfile>(altered).is_err());
        }
    }

    #[test]
    fn only_connected_password_authenticated_devices_receive_the_fixed_service() {
        let mut snapshot = NodeSnapshot::default();
        for (i, phase, password, trusted) in [
            (1, "connected", true, false),
            (2, "connected", false, true),
            (3, "disconnected", true, false),
            (4, "authorization_pending", false, false),
        ] {
            let id = Identity::generate().node_id().to_hex();
            snapshot.peers.insert(
                i.to_string(),
                PeerSnapshot {
                    node_id: id,
                    phase: phase.into(),
                    inbound_password: password,
                    inbound_trusted: trusted,
                    ..Default::default()
                },
            );
        }
        let allowed = snapshot.peers["1"].node_id.clone();
        let peers = authorized_peers(snapshot);
        assert_eq!(peers, BTreeSet::from([allowed.clone()]));
        let service = only_service(&peers);
        assert_eq!(service.allowed_peers, vec![allowed]);
        assert_eq!(service.id, "sub2api");
        assert_eq!(service.target.to_string(), TARGET);
    }

    #[test]
    fn initialization_refuses_to_overwrite_a_preexisting_identity() {
        let root =
            std::env::temp_dir().join(format!("sub2api-init-{:032x}", rand::random::<u128>()));
        private_access::ensure_directory(&root).unwrap();
        let identity = Identity::generate();
        identity.save(&root.join("identity.key")).unwrap();
        assert!(initialize_gateway(&root, "127.0.0.1:8900".into(), "a7B8c9D0e1F2".into()).is_err());
        assert_eq!(
            Identity::load(&root.join("identity.key"))
                .unwrap()
                .node_id(),
            identity.node_id()
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn two_distinct_identities_share_only_sub2api_and_wrong_password_is_denied() {
        use crate::node::core::control::NodeSessionConfig;
        use crate::node::core::dispatch;
        use crate::node::core::services;
        use crate::node::core::wire::StreamKind;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let signal = listener.local_addr().unwrap().to_string();
        let signaling = tokio::spawn(crate::discovery::signal::run_signal_server_on(listener));
        let target = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let target_address = target.local_addr().unwrap();
        let echo = tokio::spawn(async move {
            while let Ok((mut tcp, _)) = target.accept().await {
                tokio::spawn(async move {
                    let mut bytes = [0; 5];
                    if tcp.read_exact(&mut bytes).await.is_ok() {
                        let _ = tcp.write_all(&bytes).await;
                    }
                });
            }
        });
        let local = || {
            let mut c = NodeSessionConfig::new(&signal);
            c.enable_files = false;
            c.enable_speed = false;
            c.remote_auth =
                Some(RemoteVerifier::create(&SecretPassword::generate().unwrap()).unwrap());
            c.network.stun_servers.clear();
            c.network.include_loopback = true;
            c
        };
        let gateway_id = Identity::generate();
        let peer = gateway_id.node_id();
        let password = SecretPassword::new("a7B8c9D0e1F2".into()).unwrap();
        let mut config = local();
        config.remote_auth = Some(RemoteVerifier::create(&password).unwrap());
        let (gateway, mut events) = control::spawn(gateway_id, config).unwrap();
        let authorizer = gateway.clone();
        let grants = tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_millis(20));
            let mut allowed = BTreeSet::new();
            loop {
                tokio::select! {
                    event = events.recv() => if event.is_none() { break },
                    _ = interval.tick() => {
                        let next = authorized_peers(authorizer.snapshot().unwrap());
                        if next != allowed {
                            let mut service = only_service(&next);
                            service.target = target_address;
                            authorizer.update_tunnel_settings(vec![service], vec![]).unwrap();
                            allowed = next;
                        }
                    }
                }
            }
        });
        let first = Identity::generate();
        let second = Identity::generate();
        assert_ne!(first.node_id(), second.node_id());
        let (a, mut ea) = control::spawn(first, local()).unwrap();
        let (b, mut eb) = control::spawn(second, local()).unwrap();
        let drain_a = tokio::spawn(async move { while ea.recv().await.is_some() {} });
        let drain_b = tokio::spawn(async move { while eb.recv().await.is_some() {} });
        tokio::time::timeout(Duration::from_secs(10), async {
            while [&gateway, &a, &b]
                .iter()
                .any(|c| c.snapshot().unwrap().signal != "online")
            {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        for client in [&a, &b] {
            client
                .connect_peer_with_password(peer, password.clone())
                .unwrap();
            let mut updates = client.connection_updates(peer).await.unwrap();
            let connection = tokio::time::timeout(Duration::from_secs(20), async {
                loop {
                    if let Some(c) = updates.borrow().clone() {
                        break c;
                    }
                    updates.changed().await.unwrap();
                }
            })
            .await
            .unwrap_or_else(|e| {
                panic!(
                    "{e}: client={:?}, gateway={:?}",
                    client.snapshot(),
                    gateway.snapshot()
                )
            });
            let entries = tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    let entries = services::list(&connection).await.unwrap();
                    if !entries.is_empty() {
                        break entries;
                    }
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
            })
            .await
            .unwrap();
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].id, "sub2api");
            let mut stream = services::open_tcp(&connection, "sub2api").await.unwrap();
            stream.send.write_all(b"hello").await.unwrap();
            let mut reply = [0; 5];
            stream.recv.read_exact(&mut reply).await.unwrap();
            assert_eq!(&reply, b"hello");
            for forbidden in ["ubuntu-admin", "samba", "streamdesk", "other-service"] {
                assert!(services::open_tcp(&connection, forbidden).await.is_err());
            }
            for kind in [StreamKind::File, StreamKind::Speed] {
                let mut stream = dispatch::open_bi(&connection, kind).await.unwrap();
                stream.send.finish().unwrap();
                let result =
                    tokio::time::timeout(Duration::from_secs(3), stream.recv.read(&mut [0; 1]))
                        .await
                        .unwrap();
                assert!(result.is_err(), "disabled file/speed stream must be reset");
            }
        }
        assert_eq!(authorized_peers(gateway.snapshot().unwrap()).len(), 2);
        let (bad, mut bad_events) = control::spawn(Identity::generate(), local()).unwrap();
        let drain_bad = tokio::spawn(async move { while bad_events.recv().await.is_some() {} });
        tokio::time::timeout(Duration::from_secs(10), async {
            while bad.snapshot().unwrap().signal != "online" {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        bad.connect_peer_with_password(peer, SecretPassword::new("Wrong7Pass9".into()).unwrap())
            .unwrap();
        tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                if bad
                    .snapshot()
                    .unwrap()
                    .peers
                    .values()
                    .any(|p| p.phase == "failed")
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .unwrap();
        assert!(bad.list_services(peer).await.is_err());
        grants.abort();
        for client in [a, b, bad, gateway] {
            stop_control(client).await;
        }
        for task in [drain_a, drain_b, drain_bad] {
            task.abort();
        }
        signaling.abort();
        echo.abort();
    }

    #[tokio::test]
    #[ignore = "requires SUB2API_LIVE_PROFILE and an explicitly deployed isolated gateway"]
    async fn deployed_gateway_accepts_an_independent_identity_and_only_sub2api() {
        use crate::node::core::services;
        let path = std::env::var_os("SUB2API_LIVE_PROFILE").expect("private profile path required");
        let profile: ClientProfile = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        let identity = Identity::generate();
        let id = identity.node_id();
        let peer = NodeId::from_hex(&profile.server_node_id).unwrap();
        let mut session = profile
            .settings(std::env::temp_dir())
            .unwrap()
            .session()
            .unwrap();
        session.tunnel_rules.clear(); // No local listeners or existing Mac configuration.
        // The isolated Linux acceptance service can block direct UDP for its own
        // cgroup, while allowing the fixed relay. Assert the route, not just HTTP.
        let required_relay = std::env::var_os("SUB2API_LIVE_REQUIRE_RELAY").map(|_| {
            session.network.families = crate::net::NetworkFamilies::Ipv4Only;
            session.network.stun_servers.clear();
            profile
                .relay_server
                .as_ref()
                .expect("fixed relay required")
                .parse::<std::net::SocketAddr>()
                .expect("live relay must be a numeric endpoint")
        });
        let (control, mut events) = control::spawn(identity, session).unwrap();
        let drain = tokio::spawn(async move { while events.recv().await.is_some() {} });
        let mut updates = control.connection_updates(peer).await.unwrap();
        control
            .connect_peer_with_password(peer, SecretPassword::new(profile.password).unwrap())
            .unwrap();
        let connection = tokio::time::timeout(Duration::from_secs(75), async {
            loop {
                if let Some(c) = updates.borrow().clone() { break c; }
                tokio::select! {
                    result = updates.changed() => result.unwrap(),
                    _ = tokio::time::sleep(Duration::from_secs(3)) => { let _ = control.request_peer_connection(peer); }
                }
            }
        }).await.unwrap();
        if let Some(relay) = required_relay {
            assert_eq!(
                connection.remote_address(),
                relay,
                "acceptance must use relay"
            );
        }
        let entries = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let entries = services::list(&connection).await.unwrap();
                if !entries.is_empty() {
                    break entries;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, SERVICE);
        let forbidden = ["ubuntu-admin", "samba", "streamdesk"];
        for service in forbidden {
            assert!(services::open_tcp(&connection, service).await.is_err());
        }
        let mut stream = services::open_tcp(&connection, SERVICE).await.unwrap();
        stream
            .send
            .write_all(b"GET / HTTP/1.1\r\nHost: 127.0.0.1:8080\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        let mut header = [0; 12];
        tokio::time::timeout(Duration::from_secs(10), stream.recv.read_exact(&mut header))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&header, b"HTTP/1.1 200");
        println!(
            "{}",
            serde_json::json!({"client_node_id":id.to_hex(),"gateway_node_id":peer.to_hex(),"http_status":200,"visible_services":[SERVICE],"denied_services":forbidden,"existing_mac_state_modified":false})
        );
        drop(stream);
        stop_control(control).await;
        drain.abort();
    }
}
