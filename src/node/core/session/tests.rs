use std::collections::HashSet;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;

use super::*;
use crate::discovery::signal::{
    SignalServerConfig, run_signal_server_on_borrowed, run_signal_server_on_with,
};

fn auth_config(server: SocketAddr) -> NodeSessionConfig {
    let mut config = local_config(server);
    config.test_outgoing_password = None;
    config
}
async fn wait_failed(events: &mut mpsc::Receiver<SessionEvent>) {
    time::timeout(Duration::from_secs(20), async {
        while let Some(event) = events.recv().await {
            match event {
                SessionEvent::PeerState {
                    state: PeerLifecycle::Connected(_),
                    ..
                } => panic!("unauthorized peer was published"),
                SessionEvent::PeerState {
                    state: PeerLifecycle::Failed(_),
                    ..
                } => return,
                _ => {}
            }
        }
        panic!("session ended before failure");
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn ipv6_desktop_responder_authenticates_password_without_any_punch_response() {
    use crate::net::{NetworkFamilies, family::ipv6_test_available};
    if !ipv6_test_available() {
        return;
    }
    let (signal, server) = start_local_server().await;
    let desktop = Identity::generate();
    let mut client = Identity::generate();
    while !should_initiate_quic(client.node_id(), desktop.node_id()) {
        client = Identity::generate();
    }
    let mut config = auth_config(signal);
    config.network.families = NetworkFamilies::Ipv6Only;
    let (handle, mut events) = spawn(desktop.clone(), config).unwrap();
    wait_signal_online(&mut events).await;
    // A normal Quinn client never interprets/responds to PunchToken packets.
    let endpoint = crate::transport::quic::client_endpoint("[::1]:0".parse().unwrap()).unwrap();
    let mut signaling = SignalingClient::connect_with_events(
        &signal.to_string(),
        &client,
        vec![Candidate::host(endpoint.local_addr().unwrap())],
    )
    .await
    .unwrap();
    let offer = signaling
        .resolve_peer(desktop.node_id(), Duration::from_secs(5))
        .await
        .unwrap();
    time::timeout(Duration::from_secs(5), async {
            while let Some(event) = events.recv().await {
                if matches!(event, SessionEvent::PeerState { peer, state: PeerLifecycle::Authenticating, .. } if peer == client.node_id()) { return; }
            }
            panic!("desktop stopped before pending attempt");
        }).await.unwrap();
    let connection = crate::net::race::authenticated_race(
        offer
            .candidates
            .iter()
            .filter(|c| c.addr.is_ipv6())
            .map(|c| (endpoint.clone(), c.addr))
            .collect(),
        &client,
        desktop.node_id(),
    )
    .await
    .unwrap();
    let capabilities = crate::node::core::protocol::negotiate(&connection, true)
        .await
        .unwrap();
    let authorization = AuthContext::default()
        .authorize_session(
            &connection,
            true,
            client.node_id(),
            desktop.node_id(),
            test_verifier(),
            Some(test_password()),
            capabilities,
        )
        .await
        .unwrap();
    assert_eq!(
        authorization.authorization.outbound,
        crate::node::core::remote_auth::AuthorizationGrant::password(true)
    );
    assert!(!authorization.authorization.inbound_authorized());
    wait_grants(
        &mut events,
        client.node_id(),
        RemoteAuthorization {
            inbound: crate::node::core::remote_auth::AuthorizationGrant::password(true),
            outbound: Default::default(),
        },
    )
    .await;
    assert!(
        inspect(&handle).await[&client.node_id()]
            .1
            .remote_address()
            .is_ipv6()
    );
    connection.close(0u32.into(), b"done");
    handle.shutdown();
    drop(handle);
    endpoint.close(0u32.into(), b"done");
    server.abort();
}

#[tokio::test]
async fn ipv6_quic_completes_when_peer_never_answers_punch_probes() {
    use crate::net::{NetworkFamilies, family::ipv6_test_available};
    if !ipv6_test_available() {
        return;
    }
    let local = Identity::generate();
    let mut remote = Identity::generate();
    while !should_initiate_quic(local.node_id(), remote.node_id()) {
        remote = Identity::generate();
    }
    let peer = remote.node_id();
    // Ordinary Quinn endpoint ignores our PunchToken datagrams; only QUIC is answered.
    let endpoint = crate::transport::quic::server_endpoint("[::1]:0".parse().unwrap()).unwrap();
    let address = endpoint.local_addr().unwrap();
    let (committed, mut commits) = mpsc::channel(1);
    let server_endpoint = endpoint.clone();
    let responder = tokio::spawn(async move {
        let connection = server_endpoint.accept().await.unwrap().await.unwrap();
        let guard = ConnectionGuard::new(connection);
        let binding = ChannelBinding::from_connection(guard.connection()).unwrap();
        let (mut send, mut recv) = guard.connection().accept_bi().await.unwrap();
        let outcome = handshake_responder(&mut send, &mut recv, &remote, &binding)
            .await
            .unwrap();
        let _ = send.finish();
        crate::transport::quic::await_identity_commit(&mut recv)
            .await
            .unwrap();
        committed.send(outcome.peer_node_id).await.unwrap();
        let _ = guard.connection().closed().await;
    });
    let network = prepare_desktop_network(&DesktopNetworkConfig {
        families: NetworkFamilies::Ipv6Only,
        stun_servers: Vec::new(),
        include_loopback: true,
        ..Default::default()
    })
    .await
    .unwrap();
    let (_tx, incoming) = mpsc::channel(1);
    let started = time::Instant::now();
    let connection = time::timeout(
        Duration::from_secs(3),
        race_peer_paths(
            vec![(network.paths[0].clone(), vec![address])],
            PunchToken::random(),
            peer,
            1,
            local.node_id(),
            local.clone(),
            incoming,
            None,
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(started.elapsed() < Duration::from_secs(3));
    assert!(connection.connection().remote_address().is_ipv6());
    assert_eq!(
        time::timeout(Duration::from_secs(2), commits.recv())
            .await
            .unwrap(),
        Some(local.node_id())
    );
    connection.connection().close(0u32.into(), b"done");
    network.close();
    endpoint.close(0u32.into(), b"done");
    responder.await.unwrap();
    network.wait_idle().await;
}

#[test]
fn pending_ipv6_candidate_is_only_a_unique_current_generation_routing_hint() {
    let local = Identity::generate().node_id();
    let mut peer = Identity::generate().node_id();
    while should_initiate_quic(local, peer) {
        peer = Identity::generate().node_id();
    }
    let mut peers = PeerRegistry::default();
    let BeginPeerAttempt::Started(generation) = peers.begin_attempt(peer) else {
        panic!("start")
    };
    peers.transition(peer, generation, PeerLifecycle::Authenticating);
    let address = "[::1]:9000".parse().unwrap();
    let mut candidates = HashMap::from([(peer, (generation, vec![Candidate::host(address)]))]);
    assert_eq!(
        pending_ipv6_route(address, local, &peers, &candidates),
        Some((peer, generation))
    );
    assert_eq!(
        pending_ipv6_route("[::1]:9001".parse().unwrap(), local, &peers, &candidates),
        None
    );
    assert_eq!(
        pending_ipv6_route(
            "127.0.0.1:9000".parse().unwrap(),
            local,
            &peers,
            &candidates
        ),
        None
    );
    let mut other = Identity::generate().node_id();
    while should_initiate_quic(local, other) || other == peer || other == local {
        other = Identity::generate().node_id();
    }
    let BeginPeerAttempt::Started(other_generation) = peers.begin_attempt(other) else {
        panic!("other")
    };
    peers.transition(other, other_generation, PeerLifecycle::Authenticating);
    candidates.insert(other, (other_generation, vec![Candidate::host(address)]));
    assert_eq!(
        pending_ipv6_route(address, local, &peers, &candidates),
        None,
        "ambiguous source is not a routing hint"
    );
    candidates.remove(&other);
    candidates.get_mut(&peer).unwrap().0 += 1;
    assert_eq!(
        pending_ipv6_route(address, local, &peers, &candidates),
        None
    );
    candidates.get_mut(&peer).unwrap().0 = generation;
    peers.transition(
        peer,
        generation,
        PeerLifecycle::Connected(RemoteAuthorization::default()),
    );
    assert_eq!(
        pending_ipv6_route(address, local, &peers, &candidates),
        None
    );
    // A routing hint itself granted neither password nor trusted permission.
    assert_eq!(
        peers.state(peer),
        Some(&PeerLifecycle::Connected(RemoteAuthorization::default()))
    );
}

#[tokio::test]
async fn ipv6_only_concurrent_desktop_requests_publish_one_verified_transport() {
    use crate::net::{NetworkFamilies, family::ipv6_test_available};
    if !ipv6_test_available() {
        return;
    }
    let (signal, server) = start_local_server().await;
    let a = Identity::generate();
    let b = Identity::generate();
    let mut ca = local_config(signal);
    ca.network.families = NetworkFamilies::Ipv6Only;
    let cb = ca.clone();
    let (ha, mut ea) = spawn(a.clone(), ca).unwrap();
    let (hb, mut eb) = spawn(b.clone(), cb).unwrap();
    wait_signal_online(&mut ea).await;
    wait_signal_online(&mut eb).await;
    for _ in 0..4 {
        ha.connect_peer(b.node_id()).unwrap();
        hb.connect_peer(a.node_id()).unwrap();
    }
    for (events, peer) in [(&mut ea, b.node_id()), (&mut eb, a.node_id())] {
        let count = time::timeout(Duration::from_secs(20), async {
            let mut committed = 0;
            let mut typed_transport = 0;
            loop {
                match events.recv().await.expect("session alive") {
                    SessionEvent::PeerTransport {
                        peer: id,
                        relay,
                        family,
                        ..
                    } if id == peer => {
                        assert!(!relay);
                        assert_eq!(family, AddressFamily::Ipv6);
                        typed_transport += 1;
                    }
                    SessionEvent::PeerPath {
                        peer: id, detail, ..
                    } if id == peer && detail.contains("已认证 transport") => {
                        assert!(detail.contains("IPv6"));
                        committed += 1;
                    }
                    SessionEvent::PeerState {
                        peer: id,
                        state: PeerLifecycle::Connected(_),
                        ..
                    } if id == peer => {
                        assert_eq!(typed_transport, 1);
                        break committed;
                    }
                    SessionEvent::PeerState {
                        state: PeerLifecycle::Failed(error),
                        ..
                    } => panic!("{error}"),
                    _ => {}
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(
            count, 1,
            "only one transport may enter authorization/business dispatch"
        );
    }
    let a_connections = inspect(&ha).await;
    let b_connections = inspect(&hb).await;
    assert_eq!(a_connections.len(), 1);
    assert_eq!(b_connections.len(), 1);
    assert!(a_connections[&b.node_id()].1.remote_address().is_ipv6());
    assert!(b_connections[&a.node_id()].1.remote_address().is_ipv6());
    ha.shutdown();
    hb.shutdown();
    drop((ha, hb));
    server.abort();
}

#[tokio::test]
async fn trusted_auto_start_reconnects_ipv4_to_ipv6_with_new_generation_and_binding() {
    use crate::net::{NetworkFamilies, family::ipv6_test_available};
    use crate::node::core::config::{AllowedForwardTarget, DesktopConfig, TunnelRule};
    use crate::node::core::remote_auth::AuthorizationGrant;
    use crate::node::core::trusted_devices::TrustedDevice;
    if !ipv6_test_available() {
        return;
    }
    let root = std::env::temp_dir().join(format!(
        "p2p-trusted-family-switch-{:032x}",
        rand::random::<u128>()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("settings.json");
    let a = Identity::generate();
    let b = Identity::generate();
    security_config(
        &path,
        vec![TrustedDevice::new(a.node_id(), "peer".into(), None)],
    );
    let (signal, server) = start_local_server().await;
    let target = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let reserve = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let listen = reserve.local_addr().unwrap();
    drop(reserve);
    let mut rule = TunnelRule::new(
        "family recovery",
        b.node_id().to_hex(),
        listen.port(),
        target.local_addr().unwrap(),
    );
    rule.auto_start = true;
    let mut ca = auth_config(signal);
    ca.tunnel_rules = vec![rule.clone()];
    let mut cb = auth_config(signal);
    cb.network.families = NetworkFamilies::Ipv4Only;
    cb.config_path = Some(path.clone());
    cb.trusted_devices = crate::node::core::config::SettingsDraft::from_config(
        DesktopConfig::load(&path).unwrap().unwrap(),
    )
    .trusted_devices;
    cb.allowed_forward_targets = vec![AllowedForwardTarget::new(
        "echo",
        target.local_addr().unwrap(),
        vec![a.node_id().to_hex()],
    )];
    let restart = cb.clone();
    let (hb, mut eb) = spawn(b.clone(), cb).unwrap();
    wait_signal_online(&mut eb).await;
    let (ha, mut ea) = spawn(a.clone(), ca).unwrap();
    wait_specific_tunnel_state(&mut ea, &rule.id, TunnelRuntimeState::Running).await;
    let (old_generation, old_connection) = inspect(&ha).await[&b.node_id()].clone();
    assert!(old_connection.remote_address().is_ipv4());
    let old_binding = ChannelBinding::from_connection(&old_connection).unwrap();
    hb.shutdown();
    drop(hb);
    time::timeout(Duration::from_secs(10), async {
        while !inspect(&ha).await.is_empty() {
            time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(old_connection.close_reason().is_some());
    while ea.try_recv().is_ok() {}
    let mut cb = restart;
    cb.network.families = NetworkFamilies::Ipv6Only;
    cb.trusted_devices = crate::node::core::config::SettingsDraft::from_config(
        DesktopConfig::load(&path).unwrap().unwrap(),
    )
    .trusted_devices;
    let (hb, mut eb) = spawn(b.clone(), cb).unwrap();
    wait_signal_online(&mut eb).await;
    ha.connect_peer_trusted(b.node_id()).unwrap();
    wait_specific_tunnel_state(&mut ea, &rule.id, TunnelRuntimeState::Running).await;
    let (generation, connection) = inspect(&ha).await[&b.node_id()].clone();
    assert_ne!(generation, old_generation);
    assert_ne!(connection.stable_id(), old_connection.stable_id());
    assert_ne!(
        ChannelBinding::from_connection(&connection).unwrap(),
        old_binding
    );
    assert!(connection.remote_address().is_ipv6());
    wait_grants(
        &mut eb,
        a.node_id(),
        RemoteAuthorization {
            inbound: AuthorizationGrant {
                password: false,
                trusted_device: true,
            },
            outbound: AuthorizationGrant::default(),
        },
    )
    .await;
    let mut tcp = tokio::net::TcpStream::connect(listen).await.unwrap();
    tcp.write_all(b"ipv6new").await.unwrap();
    let (mut destination, _) = time::timeout(Duration::from_secs(3), target.accept())
        .await
        .unwrap()
        .unwrap();
    let mut data = [0; 7];
    destination.read_exact(&mut data).await.unwrap();
    assert_eq!(&data, b"ipv6new");
    // Address-family recovery did not mutate either password verifier or persistent trust.
    assert_eq!(
        crate::node::core::config::SettingsDraft::from_config(
            DesktopConfig::load(&path).unwrap().unwrap()
        )
        .trusted_devices
        .len(),
        1
    );
    ha.shutdown();
    hb.shutdown();
    drop((ha, hb, tcp, destination));
    server.abort();
    std::fs::remove_dir_all(root).unwrap();
}
#[tokio::test]
async fn relay_authenticated_late_generation_is_closed_by_the_actual_publication_gate() {
    let socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let address = socket.local_addr().unwrap();
    let admission = crate::relay::server::Admission::new(Default::default()).unwrap();
    let server = tokio::spawn(crate::relay::server::run(socket, admission.clone()));
    let a = Identity::generate();
    let b = Identity::generate();
    let peer = b.node_id();
    let local = a.node_id();
    let token = PunchToken::random();
    assert!(admission.issue(token, local, peer));
    let responder = tokio::spawn(async move {
        crate::relay::client::prepare(&b, local, token, address)
            .await
            .unwrap()
            .wait_for_selection()
            .await
            .unwrap()
    });
    let mut guard = crate::relay::client::prepare(&a, peer, token, address)
        .await
        .unwrap()
        .finish()
        .await
        .unwrap();
    let remote = responder.await.unwrap();
    let pool = crate::relay::client::EndpointPool::default();
    guard.retain_relay_endpoint(&pool).unwrap();
    let mut peers = PeerRegistry::default();
    let BeginPeerAttempt::Started(old) = peers.begin_attempt(peer) else {
        panic!()
    };
    peers.transition(peer, old, PeerLifecycle::Disconnected);
    let BeginPeerAttempt::Started(new) = peers.begin_attempt(peer) else {
        panic!()
    };
    assert!(!transport_can_publish(
        &peers,
        peer,
        old,
        guard.connection()
    ));
    assert_eq!(peers.generation(peer), Some(new));
    time::timeout(Duration::from_secs(2), remote.connection().closed())
        .await
        .unwrap();
    pool.close();
    drop((guard, remote));
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn relay_pairing_before_close_keeps_live_winner_then_reconnects_on_fresh_tls() {
    async fn deferred(handle: &NodeSessionHandle, peer: NodeId) -> Option<PunchToken> {
        let (tx, rx) = oneshot::channel();
        handle
            .commands
            .send(SessionCommand::InspectDeferred(peer, tx))
            .await
            .unwrap();
        time::timeout(Duration::from_secs(2), rx)
            .await
            .unwrap()
            .unwrap()
    }
    async fn with_route(
        events: &mut mpsc::Receiver<SessionEvent>,
        peer: NodeId,
    ) -> RemoteAuthorization {
        time::timeout(Duration::from_secs(20), async {
            let mut route = false;
            loop {
                match events.recv().await.expect("session active") {
                    SessionEvent::PeerTransport {
                        peer: id,
                        relay,
                        family,
                        ..
                    } if id == peer => {
                        assert!(relay);
                        assert_eq!(family, AddressFamily::Ipv4);
                        route = true;
                    }
                    SessionEvent::PeerState {
                        peer: id,
                        state: PeerLifecycle::Connected(auth),
                        ..
                    } if id == peer => {
                        assert!(
                            route,
                            "authenticated Relay route precedes session authorization"
                        );
                        return auth;
                    }
                    SessionEvent::PeerState {
                        state: PeerLifecycle::Failed(error),
                        ..
                    } => panic!("{error}"),
                    _ => {}
                }
            }
        })
        .await
        .unwrap()
    }
    let (signal, relay, server) = crate::relay::tests::signaling_fixture().await;
    let a = Identity::generate();
    let b = Identity::generate();
    let blackhole_a = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let blackhole_b = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let mut ca = auth_config(signal);
    let mut cb = auth_config(signal);
    for (config, blackhole) in [(&mut ca, &blackhole_a), (&mut cb, &blackhole_b)] {
        config.network.families = crate::net::NetworkFamilies::Ipv4Only;
        config.network.relay_server = Some(relay.to_string());
        config.network.advertise_only = true;
        config.network.advertise = vec![blackhole.local_addr().unwrap()];
    }
    let (ha, mut ea) = spawn(a.clone(), ca).unwrap();
    let (hb, mut eb) = spawn(b.clone(), cb).unwrap();
    wait_signal_online(&mut ea).await;
    wait_signal_online(&mut eb).await;
    ha.connect_peer_with_password(b.node_id(), test_password())
        .unwrap();
    let rights_a = with_route(&mut ea, b.node_id()).await;
    let rights_b = with_route(&mut eb, a.node_id()).await;
    let (old_generation, old) = inspect(&ha).await[&b.node_id()].clone();
    let old_b = inspect(&hb).await[&a.node_id()].1.clone();
    let binding = ChannelBinding::from_connection(&old).unwrap();
    ha.connect_peer(b.node_id()).unwrap(); // Actual server signs/registers/pairs; no synthetic token.
    time::timeout(Duration::from_secs(3), async {
        loop {
            if let (Some(a), Some(b)) = (
                deferred(&ha, b.node_id()).await,
                deferred(&hb, a.node_id()).await,
            ) && a == b
            {
                break;
            }
            time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        inspect(&ha).await[&b.node_id()].1.stable_id(),
        old.stable_id()
    );
    assert_eq!(
        inspect(&hb).await[&a.node_id()].1.stable_id(),
        old_b.stable_id()
    );
    assert!(old.close_reason().is_none()); // Signaling alone cannot replace a live winner.
    while ea.try_recv().is_ok() {}
    while eb.try_recv().is_ok() {}
    old_b.close(0u32.into(), b"force pairing-before-close ordering");
    assert_eq!(with_route(&mut ea, b.node_id()).await, rights_a);
    assert_eq!(with_route(&mut eb, a.node_id()).await, rights_b);
    let (generation, fresh) = inspect(&ha).await[&b.node_id()].clone();
    assert_ne!(generation, old_generation);
    assert_eq!(fresh.remote_address(), relay);
    assert_ne!(ChannelBinding::from_connection(&fresh).unwrap(), binding);
    assert_eq!(inspect(&ha).await.len(), 1);
    ha.shutdown();
    hb.shutdown();
    drop((ha, hb));
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn relay_password_grants_are_directional_fresh_and_wrong_password_never_connects() {
    use crate::node::core::remote_auth::AuthorizationGrant;
    let (signal, relay, server) = crate::relay::tests::signaling_fixture().await;
    let a = Identity::generate();
    let b = Identity::generate();
    let blackhole_a = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let blackhole_b = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let mut ca = auth_config(signal);
    let mut cb = auth_config(signal);
    for (config, blackhole) in [(&mut ca, &blackhole_a), (&mut cb, &blackhole_b)] {
        config.network.families = crate::net::NetworkFamilies::Ipv4Only;
        config.network.relay_server = Some(relay.to_string());
        config.network.advertise_only = true;
        config.network.advertise = vec![blackhole.local_addr().unwrap()];
    }
    let restart = cb.clone();
    let (ha, mut ea) = spawn(a.clone(), ca).unwrap();
    let (hb, mut eb) = spawn(b.clone(), cb).unwrap();
    wait_signal_online(&mut ea).await;
    wait_signal_online(&mut eb).await;
    ha.connect_peer_trusted(b.node_id()).unwrap(); // No implicit trust from RelayReady.
    wait_failed(&mut ea).await;
    wait_failed(&mut eb).await;
    assert!(inspect(&ha).await.is_empty());
    ha.connect_peer_with_password(b.node_id(), SecretPassword::new("Wrong9".into()).unwrap())
        .unwrap();
    wait_failed(&mut ea).await;
    wait_failed(&mut eb).await;
    assert!(inspect(&ha).await.is_empty());
    assert!(inspect(&hb).await.is_empty());
    ha.connect_peer_with_password(b.node_id(), test_password())
        .unwrap();
    let rights = wait_authorization(&mut ea, b.node_id()).await;
    assert_eq!(
        rights,
        RemoteAuthorization {
            inbound: AuthorizationGrant::default(),
            outbound: AuthorizationGrant::password(true)
        }
    );
    let rights = wait_authorization(&mut eb, a.node_id()).await;
    assert_eq!(
        rights,
        RemoteAuthorization {
            inbound: AuthorizationGrant::password(true),
            outbound: AuthorizationGrant::default()
        }
    );
    let (old_generation, old) = inspect(&ha).await[&b.node_id()].clone();
    assert_eq!(old.remote_address(), relay);
    let binding = ChannelBinding::from_connection(&old).unwrap();
    hb.shutdown();
    drop(hb);
    time::timeout(Duration::from_secs(10), async {
        while !inspect(&ha).await.is_empty() {
            time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    while ea.try_recv().is_ok() {}
    let (hb, mut eb) = spawn(b.clone(), restart).unwrap();
    wait_signal_online(&mut eb).await;
    ha.connect_peer_trusted(b.node_id()).unwrap(); // Previous Password grant does not survive.
    wait_failed(&mut ea).await;
    wait_failed(&mut eb).await;
    assert!(inspect(&ha).await.is_empty());
    ha.connect_peer_with_password(b.node_id(), test_password())
        .unwrap();
    let rights = wait_authorization(&mut ea, b.node_id()).await;
    assert!(rights.outbound.password);
    let (generation, new) = inspect(&ha).await[&b.node_id()].clone();
    assert_ne!(generation, old_generation);
    assert_ne!(ChannelBinding::from_connection(&new).unwrap(), binding);
    assert_eq!(inspect(&ha).await.len(), 1);
    assert!(old.close_reason().is_some());
    ha.shutdown();
    hb.shutdown();
    drop((ha, hb));
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn relay_trusted_auto_start_direct_relay_direct_uses_fresh_tls_and_generation() {
    use crate::net::NetworkFamilies;
    use crate::node::core::config::{AllowedForwardTarget, DesktopConfig, TunnelRule};
    use crate::node::core::remote_auth::AuthorizationGrant;
    use crate::node::core::trusted_devices::TrustedDevice;
    let root = std::env::temp_dir().join(format!(
        "p2p-trusted-relay-switch-{:032x}",
        rand::random::<u128>()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("settings.json");
    let mut a = Identity::generate();
    let mut b = Identity::generate();
    if a.node_id() > b.node_id() {
        std::mem::swap(&mut a, &mut b);
    }
    security_config(
        &path,
        vec![TrustedDevice::new(a.node_id(), "peer".into(), None)],
    );
    let (signal, relay, server) = crate::relay::tests::signaling_fixture().await;
    let blackhole_a = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let blackhole_b = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let target = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let reserve = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let listen = reserve.local_addr().unwrap();
    drop(reserve);
    let mut rule = TunnelRule::new(
        "family recovery",
        b.node_id().to_hex(),
        listen.port(),
        target.local_addr().unwrap(),
    );
    rule.auto_start = true;
    let mut ca = auth_config(signal);
    ca.tunnel_rules = vec![rule.clone()];
    ca.network.families = NetworkFamilies::Ipv4Only;
    ca.network.relay_server = Some(relay.to_string());
    ca.network.advertise_only = true;
    ca.network.advertise = vec![blackhole_a.local_addr().unwrap()];
    let mut cb = auth_config(signal);
    cb.network.families = NetworkFamilies::Ipv4Only;
    cb.network.relay_server = Some(relay.to_string());
    cb.config_path = Some(path.clone());
    cb.trusted_devices = crate::node::core::config::SettingsDraft::from_config(
        DesktopConfig::load(&path).unwrap().unwrap(),
    )
    .trusted_devices;
    cb.allowed_forward_targets = vec![AllowedForwardTarget::new(
        "echo",
        target.local_addr().unwrap(),
        vec![a.node_id().to_hex()],
    )];
    let restart = cb.clone();
    let (hb, mut eb) = spawn(b.clone(), cb).unwrap();
    wait_signal_online(&mut eb).await;
    let (ha, mut ea) = spawn(a.clone(), ca).unwrap();
    wait_specific_tunnel_state(&mut ea, &rule.id, TunnelRuntimeState::Running).await;
    let (old_generation, old_connection) = inspect(&ha).await[&b.node_id()].clone();
    assert!(old_connection.remote_address().is_ipv4());
    let old_binding = ChannelBinding::from_connection(&old_connection).unwrap();
    hb.shutdown();
    drop(hb);
    time::timeout(Duration::from_secs(10), async {
        while !inspect(&ha).await.is_empty() {
            time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(old_connection.close_reason().is_some());
    while ea.try_recv().is_ok() {}
    let mut cb = restart.clone();
    cb.network.advertise_only = true;
    cb.network.advertise = vec![blackhole_b.local_addr().unwrap()];
    cb.trusted_devices = crate::node::core::config::SettingsDraft::from_config(
        DesktopConfig::load(&path).unwrap().unwrap(),
    )
    .trusted_devices;
    let (hb, mut eb) = spawn(b.clone(), cb).unwrap();
    wait_signal_online(&mut eb).await;
    ha.connect_peer_trusted(b.node_id()).unwrap();
    wait_specific_tunnel_state(&mut ea, &rule.id, TunnelRuntimeState::Running).await;
    let (generation, connection) = inspect(&ha).await[&b.node_id()].clone();
    assert_ne!(generation, old_generation);
    assert_ne!(connection.stable_id(), old_connection.stable_id());
    assert_ne!(
        ChannelBinding::from_connection(&connection).unwrap(),
        old_binding
    );
    assert_eq!(connection.remote_address(), relay);
    wait_grants(
        &mut eb,
        a.node_id(),
        RemoteAuthorization {
            inbound: AuthorizationGrant {
                password: false,
                trusted_device: true,
            },
            outbound: AuthorizationGrant::default(),
        },
    )
    .await;
    let mut tcp = tokio::net::TcpStream::connect(listen).await.unwrap();
    tcp.write_all(b"relay!!").await.unwrap();
    let (mut destination, _) = time::timeout(Duration::from_secs(3), target.accept())
        .await
        .unwrap()
        .unwrap();
    let mut data = [0; 7];
    destination.read_exact(&mut data).await.unwrap();
    assert_eq!(&data, b"relay!!");
    // Address-family recovery did not mutate either password verifier or persistent trust.
    assert_eq!(
        crate::node::core::config::SettingsDraft::from_config(
            DesktopConfig::load(&path).unwrap().unwrap()
        )
        .trusted_devices
        .len(),
        1
    );
    let relay_generation = generation;
    let relay_binding = ChannelBinding::from_connection(&connection).unwrap();
    hb.shutdown();
    drop(hb);
    time::timeout(Duration::from_secs(10), async {
        while !inspect(&ha).await.is_empty() {
            time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    drop((tcp, destination));
    while ea.try_recv().is_ok() {}
    let (hb, mut eb) = spawn(b.clone(), restart).unwrap();
    wait_signal_online(&mut eb).await;
    ha.connect_peer_trusted(b.node_id()).unwrap();
    wait_specific_tunnel_state(&mut ea, &rule.id, TunnelRuntimeState::Running).await;
    let (generation, direct) = inspect(&ha).await[&b.node_id()].clone();
    assert_ne!(generation, relay_generation);
    assert_ne!(
        ChannelBinding::from_connection(&direct).unwrap(),
        relay_binding
    );
    assert_ne!(direct.remote_address(), relay);
    assert!(connection.close_reason().is_some());
    assert_eq!(inspect(&ha).await.len(), 1);
    ha.shutdown();
    hb.shutdown();
    drop((ha, hb));
    server.abort();
    std::fs::remove_dir_all(root).unwrap();
}
#[tokio::test]
async fn ipv6_and_ipv4_reconnect_routes_retire_together_and_old_generation_is_rejected() {
    use crate::net::family::{bind_udp, ipv6_test_available};
    if !ipv6_test_available() {
        return;
    }
    let network = prepare_desktop_network(&DesktopNetworkConfig {
        stun_servers: Vec::new(),
        include_loopback: true,
        ..Default::default()
    })
    .await
    .unwrap();
    let peer = Identity::generate().node_id();
    let old_token = PunchToken::random();
    let mut peers = PeerRegistry::default();
    let BeginPeerAttempt::Started(old_generation) = peers.begin_attempt(peer) else {
        panic!("start")
    };
    let mut receivers = Vec::new();
    let mut sources = Vec::new();
    for path in &network.paths {
        let mut receiver = path
            .punch_socket
            .register_peer_probe(&old_token, peer, old_generation)
            .unwrap();
        let source =
            tokio::net::UdpSocket::from_std(bind_udp(path.family.loopback(0)).unwrap()).unwrap();
        source
            .send_to(
                &crate::nat::punch::probe_packet(&old_token),
                path.family.loopback(path.local_addr.port()),
            )
            .await
            .unwrap();
        assert_eq!(
            time::timeout(Duration::from_secs(2), receiver.recv())
                .await
                .unwrap(),
            Some(source.local_addr().unwrap())
        );
        receivers.push(receiver);
        sources.push(source);
    }
    assert!(peers.transition(
        peer,
        old_generation,
        PeerLifecycle::Failed("retired".into())
    ));
    let BeginPeerAttempt::Started(new_generation) = peers.begin_attempt(peer) else {
        panic!("restart")
    };
    for (path, source) in network.paths.iter().zip(&sources) {
        let (_, claimed_generation) = path
            .punch_socket
            .claim_authorized_peer(source.local_addr().unwrap())
            .unwrap();
        assert_eq!(claimed_generation, old_generation);
        assert!(!peers.is_current(peer, claimed_generation));
    }
    drop(receivers); // removes both families' token/source routes synchronously
    let token = PunchToken::random();
    for (path, source) in network.paths.iter().zip(&sources) {
        assert_eq!(
            path.punch_socket
                .claim_authorized_peer(source.local_addr().unwrap()),
            None
        );
        let mut receiver = path
            .punch_socket
            .register_peer_probe(&token, peer, new_generation)
            .unwrap();
        let target = path.family.loopback(path.local_addr.port());
        source
            .send_to(&crate::nat::punch::probe_packet(&old_token), target)
            .await
            .unwrap();
        assert!(
            time::timeout(Duration::from_millis(30), receiver.recv())
                .await
                .is_err()
        );
        source
            .send_to(&crate::nat::punch::probe_packet(&token), target)
            .await
            .unwrap();
        assert!(
            time::timeout(Duration::from_secs(2), receiver.recv())
                .await
                .unwrap()
                .is_some()
        );
        assert_eq!(
            path.punch_socket
                .claim_authorized_peer(source.local_addr().unwrap()),
            Some((peer, new_generation))
        );
    }
    network.close();
    network.wait_idle().await;
}

#[tokio::test]
async fn reconnect_watch_only_accepts_matching_probe_and_expires_without_reconnecting() {
    use crate::nat::punch::probe_packet;
    use crate::transport::quic::endpoint_from_socket_with_punch_dispatcher;
    let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let address = socket.local_addr().unwrap();
    let (endpoint, punch) = endpoint_from_socket_with_punch_dispatcher(socket).unwrap();
    let peer = Identity::generate().node_id();
    let token = PunchToken::random();
    let mut watches = HashMap::from([(
        peer,
        VecDeque::from([ReconnectProbe {
            generation: 7,
            candidates: Vec::new(),
            token,
            receivers: vec![punch.register_peer_probe(&token, peer, 7).unwrap()],
            deadline: Box::pin(time::sleep(Duration::from_secs(5))),
        }]),
    )]);
    let sender = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    sender
        .send_to(&probe_packet(&PunchToken::random()), address)
        .await
        .unwrap();
    assert!(
        time::timeout(
            Duration::from_millis(100),
            std::future::poll_fn(|cx| poll_reconnect_probes(&mut watches, cx))
        )
        .await
        .is_err()
    );
    sender
        .send_to(&probe_packet(&token), address)
        .await
        .unwrap();
    assert_eq!(
        time::timeout(
            Duration::from_secs(2),
            std::future::poll_fn(|cx| { poll_reconnect_probes(&mut watches, cx) })
        )
        .await
        .unwrap(),
        (peer, 0, true)
    );
    assert_eq!(
        punch.claim_authorized_peer(sender.local_addr().unwrap()),
        Some((peer, 7))
    );
    watches.clear();
    assert_eq!(
        punch.claim_authorized_peer(sender.local_addr().unwrap()),
        None
    );
    let receiver = punch.register_peer_probe(&token, peer, 8).unwrap();
    watches.insert(
        peer,
        VecDeque::from([ReconnectProbe {
            generation: 8,
            candidates: Vec::new(),
            token,
            receivers: vec![receiver],
            deadline: Box::pin(time::sleep(Duration::ZERO)),
        }]),
    );
    assert_eq!(
        std::future::poll_fn(|cx| poll_reconnect_probes(&mut watches, cx)).await,
        (peer, 0, false)
    );
    watches.clear();
    endpoint.close(0u32.into(), b"done");
    endpoint.wait_idle().await;
}

#[tokio::test]
async fn incremental_preview_confirm_and_cancel_run_through_authenticated_session_commands() {
    use crate::node::core::task_model::TaskState;
    use crate::node::core::task_store::TaskStore;
    use crate::node::core::transfer::TransferService;
    let root = std::env::temp_dir().join(format!(
        "p2p-session-incremental-{}",
        crate::node::core::task_model::TaskId::generate()
    ));
    for dir in ["a-receive", "b-receive", "目录", "b-receive/目录"] {
        std::fs::create_dir_all(root.join(dir)).unwrap();
    }
    std::fs::write(root.join("目录/a.txt"), b"same").unwrap();
    std::fs::write(root.join("b-receive/目录/a.txt"), b"same").unwrap();
    let (a_store, _) = TaskStore::open(&root.join("a-state/tasks.json")).unwrap();
    let (b_store, _) = TaskStore::open(&root.join("b-state/tasks.json")).unwrap();
    let a_service = TransferService::new(a_store, root.join("a-receive"));
    let b_service = TransferService::new(b_store, root.join("b-receive"));
    let (address, server) = start_local_server().await;
    let a = Identity::generate();
    let b = Identity::generate();
    let mut ca = auth_config(address);
    let mut cb = auth_config(address);
    ca.transfer = Some(a_service.clone());
    cb.transfer = Some(b_service.clone());
    let (ha, mut ea) = spawn(a.clone(), ca).unwrap();
    let (hb, mut eb) = spawn(b.clone(), cb).unwrap();
    wait_signal_online(&mut ea).await;
    wait_signal_online(&mut eb).await;
    ha.connect_peer_with_password(b.node_id(), test_password())
        .unwrap();
    assert!(
        wait_authorization(&mut ea, b.node_id())
            .await
            .outbound_authorized()
    );
    assert!(
        wait_authorization(&mut eb, a.node_id())
            .await
            .inbound_authorized()
    );
    async fn ready(
        events: &mut mpsc::Receiver<SessionEvent>,
        request: &crate::node::core::task_model::TaskId,
    ) -> crate::node::core::incremental::Preview {
        time::timeout(Duration::from_secs(10), async {
            loop {
                match events.recv().await.unwrap() {
                    SessionEvent::DirectoryPreview(p) if &p.id == request => return p,
                    SessionEvent::DirectoryPreviewFailed(id) if &id == request => {
                        panic!("directory preview failed")
                    }
                    _ => {}
                }
            }
        })
        .await
        .unwrap()
    }
    let request = ha
        .preview_directory(b.node_id(), root.join("目录"))
        .unwrap();
    let preview = ready(&mut ea, &request).await;
    assert!(preview.supported);
    assert_eq!(preview.unchanged, 1);
    assert!(a_service.snapshot().await.unwrap().is_empty());
    assert!(b_service.snapshot().await.unwrap().is_empty());
    ha.cancel_directory_preview(preview.id).unwrap();
    let request = ha
        .preview_directory(b.node_id(), root.join("目录"))
        .unwrap();
    let preview = ready(&mut ea, &request).await;
    ha.confirm_directory(b.node_id(), preview.id).unwrap();
    time::timeout(Duration::from_secs(10), async {
        loop {
            let records = a_service.snapshot().await.unwrap();
            if records.len() == 2 && records.iter().all(|r| r.state() == TaskState::Completed) {
                break;
            }
            time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        b_service
            .snapshot()
            .await
            .unwrap()
            .iter()
            .any(|r| r.file_details().is_some_and(|d| d.skipped_existing))
    );
    assert_eq!(
        std::fs::read_dir(root.join("b-receive/目录"))
            .unwrap()
            .count(),
        1
    );
    ha.shutdown();
    hb.shutdown();
    drop(ha);
    drop(hb);
    server.abort();
    drop(a_service);
    drop(b_service);
    std::fs::remove_dir_all(root).unwrap();
}
#[tokio::test]
async fn unauthorized_file_directory_resume_and_speed_commands_cannot_start_business() {
    use crate::node::core::task_store::TaskStore;
    use crate::node::core::transfer::TransferService;
    let root = std::env::temp_dir().join(format!("p2p-auth-gate-{:032x}", rand::random::<u128>()));
    std::fs::create_dir(&root).unwrap();
    std::fs::create_dir(root.join("receive")).unwrap();
    let (store, _) = TaskStore::open(&root.join("state/tasks.json")).unwrap();
    let service = TransferService::new(store, root.join("receive"));
    let (address, server) = start_local_server().await;
    let mut config = auth_config(address);
    config.transfer = Some(service.clone());
    let (handle, mut events) = spawn(Identity::generate(), config).unwrap();
    wait_signal_online(&mut events).await;
    let peer = Identity::generate().node_id();
    let source = root.join("source.txt");
    std::fs::write(&source, b"sensitive").unwrap();
    handle.send_file(peer, source).unwrap();
    handle.send_directory(peer, root.join("receive")).unwrap();
    handle
        .preview_directory(peer, root.join("receive"))
        .unwrap();
    handle
        .confirm_directory(peer, crate::node::core::task_model::TaskId::generate())
        .unwrap();
    handle
        .resume_task(peer, crate::node::core::task_model::TaskId::generate())
        .unwrap();
    handle
        .start_speed(
            peer,
            crate::node::core::config::SpeedtestDirection::Upload,
            30,
        )
        .unwrap();
    time::timeout(Duration::from_secs(3), async {
        let mut rejected = 0;
        while rejected < 6 {
            if let Some(SessionEvent::Diagnostic(message)) = events.recv().await
                && message.contains("远程访问尚未授权")
            {
                rejected += 1;
            }
        }
    })
    .await
    .unwrap();
    assert!(service.snapshot().await.unwrap().is_empty());
    assert!(inspect(&handle).await.is_empty());
    assert_eq!(std::fs::read_dir(root.join("receive")).unwrap().count(), 0);
    handle.shutdown();
    drop(handle);
    server.abort();
    let _ = server.await;
    drop(service);
    std::fs::remove_dir_all(root).unwrap();
}
#[tokio::test]
async fn wrong_password_never_publishes_business_connection_or_running_tunnel() {
    let (address, server) = start_local_server().await;
    let a = Identity::generate();
    let b = Identity::generate();
    let target = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let reserve = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let local = reserve.local_addr().unwrap();
    drop(reserve);
    let mut config = auth_config(address);
    let mut rule = crate::node::core::config::TunnelRule::new(
        "protected",
        b.node_id().to_hex(),
        local.port(),
        target.local_addr().unwrap(),
    );
    rule.auto_start = true;
    let rule_id = rule.id.clone();
    config.tunnel_rules.push(rule);
    let mut destination = auth_config(address);
    destination
        .allowed_forward_targets
        .push(crate::node::core::config::AllowedForwardTarget::new(
            "protected",
            target.local_addr().unwrap(),
            vec![a.node_id().to_hex()],
        ));
    let (ha, mut ea) = spawn(a.clone(), config).unwrap();
    let (hb, mut eb) = spawn(b.clone(), destination).unwrap();
    // A restarted process has no peer password and must not bind or attempt authentication.
    wait_specific_tunnel_state(&mut ea, &rule_id, TunnelRuntimeState::WaitingAuthorization).await;
    assert!(tokio::net::TcpStream::connect(local).await.is_err());
    wait_signal_online(&mut ea).await;
    wait_signal_online(&mut eb).await;
    assert!(inspect(&ha).await.is_empty());
    assert!(inspect(&hb).await.is_empty());
    ha.connect_peer_with_password(b.node_id(), SecretPassword::new("Wrong9".into()).unwrap())
        .unwrap();
    wait_failed(&mut ea).await;
    wait_failed(&mut eb).await;
    assert!(inspect(&ha).await.is_empty());
    assert!(inspect(&hb).await.is_empty());
    assert!(
        time::timeout(Duration::from_secs(1), target.accept())
            .await
            .is_err()
    );
    // Allow all lifecycle notifications to settle, then verify the listener has actually gone.
    ha.revoke_tunnel_rule(rule_id, true).await.unwrap();
    assert!(tokio::net::TcpStream::connect(local).await.is_err());
    ha.shutdown();
    hb.shutdown();
    server.abort();
}
async fn wait_authorization(
    events: &mut mpsc::Receiver<SessionEvent>,
    peer: NodeId,
) -> RemoteAuthorization {
    time::timeout(Duration::from_secs(20), async {
        while let Some(event) = events.recv().await {
            match event {
                SessionEvent::PeerState {
                    peer: id,
                    state: PeerLifecycle::Connected(auth),
                    ..
                } if id == peer => return auth,
                SessionEvent::PeerState {
                    state: PeerLifecycle::Failed(e),
                    ..
                } => panic!("authorization failed: {e}"),
                _ => {}
            }
        }
        panic!("session closed before authorization")
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn passwordless_auto_start_waits_without_binding_then_continues_after_explicit_auth() {
    let (address, server) = start_local_server().await;
    let a = Identity::generate();
    let b = Identity::generate();
    let reserve = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let local = reserve.local_addr().unwrap();
    drop(reserve);
    let target = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut rule = crate::node::core::config::TunnelRule::new(
        "deferred",
        b.node_id().to_hex(),
        local.port(),
        target.local_addr().unwrap(),
    );
    rule.auto_start = true;
    let mut ca = auth_config(address);
    ca.tunnel_rules.push(rule.clone());
    let mut cb = auth_config(address);
    cb.allowed_forward_targets
        .push(crate::node::core::config::AllowedForwardTarget::new(
            "target",
            target.local_addr().unwrap(),
            vec![a.node_id().to_hex()],
        ));
    let (ha, mut ea) = spawn(a.clone(), ca).unwrap();
    let (hb, mut eb) = spawn(b.clone(), cb).unwrap();
    wait_specific_tunnel_state(&mut ea, &rule.id, TunnelRuntimeState::WaitingAuthorization).await;
    wait_signal_online(&mut ea).await;
    wait_signal_online(&mut eb).await;
    assert!(tokio::net::TcpStream::connect(local).await.is_err());
    assert!(inspect(&ha).await.is_empty());
    assert!(inspect(&hb).await.is_empty());
    ha.connect_peer_with_password(b.node_id(), test_password())
        .unwrap();
    // The automatic no-password attempt may already have reported failure.
    // Verify this explicit password request's real grant before waiting on the listener.
    wait_grants(
        &mut ea,
        b.node_id(),
        RemoteAuthorization {
            inbound: crate::node::core::remote_auth::AuthorizationGrant::default(),
            outbound: crate::node::core::remote_auth::AuthorizationGrant::password(true),
        },
    )
    .await;
    wait_specific_tunnel_state(&mut ea, &rule.id, TunnelRuntimeState::Running).await;
    wait_grants(
        &mut eb,
        a.node_id(),
        RemoteAuthorization {
            inbound: crate::node::core::remote_auth::AuthorizationGrant::password(true),
            outbound: crate::node::core::remote_auth::AuthorizationGrant::password(false),
        },
    )
    .await;
    let mut tcp = tokio::net::TcpStream::connect(local).await.unwrap();
    tcp.write_all(b"request").await.unwrap();
    let (mut destination, _) = time::timeout(Duration::from_secs(5), target.accept())
        .await
        .unwrap()
        .unwrap();
    let mut bytes = [0; 7];
    destination.read_exact(&mut bytes).await.unwrap();
    assert_eq!(&bytes, b"request");
    destination.write_all(b"reply").await.unwrap();
    let mut reply = [0; 5];
    tcp.read_exact(&mut reply).await.unwrap();
    assert_eq!(&reply, b"reply");
    ha.revoke_tunnel_rule(rule.id, true).await.unwrap();
    assert!(tokio::net::TcpStream::connect(local).await.is_err());
    ha.shutdown();
    hb.shutdown();
    server.abort();
}

#[tokio::test]
async fn stopping_or_reconfiguring_waiting_rules_cancels_their_deferred_start() {
    let (address, server) = start_local_server().await;
    let a = Identity::generate();
    let b = Identity::generate();
    let target = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut reservations = Vec::new();
    let mut rules = Vec::new();
    for name in ["active", "stop", "disable", "delete", "no-auto"] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut rule = crate::node::core::config::TunnelRule::new(
            name,
            b.node_id().to_hex(),
            listener.local_addr().unwrap().port(),
            target.local_addr().unwrap(),
        );
        rule.auto_start = true;
        rules.push(rule);
        reservations.push(listener);
    }
    let mut ca = auth_config(address);
    ca.tunnel_rules = rules.clone();
    let (ha, mut ea) = spawn(a.clone(), ca).unwrap();
    let (hb, mut eb) = spawn(b.clone(), auth_config(address)).unwrap();
    for rule in &rules {
        wait_specific_tunnel_state(&mut ea, &rule.id, TunnelRuntimeState::WaitingAuthorization)
            .await;
    }
    drop(reservations);
    wait_signal_online(&mut ea).await;
    wait_signal_online(&mut eb).await;
    let mut next = rules.clone();
    next[2].enabled = false;
    next[4].auto_start = false;
    next.remove(3);
    ha.update_tunnel_settings(Vec::new(), next).unwrap();
    ha.stop_tunnel_rule(rules[1].id.clone()).unwrap();
    assert!(inspect(&ha).await.is_empty()); // Command barrier before authentication.
    ha.connect_peer_with_password(b.node_id(), test_password())
        .unwrap();
    // The automatic no-password attempt may already have reported failure.
    // Verify this explicit password request's real grant before waiting on the listener.
    wait_grants(
        &mut ea,
        b.node_id(),
        RemoteAuthorization {
            inbound: crate::node::core::remote_auth::AuthorizationGrant::default(),
            outbound: crate::node::core::remote_auth::AuthorizationGrant::password(true),
        },
    )
    .await;
    wait_specific_tunnel_state(&mut ea, &rules[0].id, TunnelRuntimeState::Running).await;
    for rule in &rules[1..] {
        assert!(
            tokio::net::TcpStream::connect(rule.listen).await.is_err(),
            "cancelled rule {} still listening",
            rule.name
        );
    }
    ha.revoke_tunnel_rule(rules[0].id.clone(), true)
        .await
        .unwrap();
    ha.shutdown();
    hb.shutdown();
    server.abort();
}

#[tokio::test]
async fn one_way_auth_gates_commands_queue_and_remote_requests_then_both_passwords_enable_reverse_business()
 {
    directional_business_fixture(false).await;
}
#[tokio::test]
async fn relay_one_way_auth_and_both_passwords_gate_bidirectional_files_and_tunnels() {
    directional_business_fixture(true).await;
}
async fn directional_business_fixture(relay_enabled: bool) {
    use crate::node::core::protocol::{self, Frame, Message};
    use crate::node::core::task_model::{TaskDirection, TaskState};
    use crate::node::core::task_store::TaskStore;
    use crate::node::core::transfer::TransferService;
    let root = std::env::temp_dir().join(format!("p2p-directional-{}", rand::random::<u128>()));
    std::fs::create_dir_all(root.join("a-receive")).unwrap();
    std::fs::create_dir(root.join("b-receive")).unwrap();
    let (sa, _) = TaskStore::open(&root.join("a-state/tasks.json")).unwrap();
    let (sb, _) = TaskStore::open(&root.join("b-state/tasks.json")).unwrap();
    let service_a = TransferService::new(sa, root.join("a-receive"));
    let service_b = TransferService::new(sb, root.join("b-receive"));
    let a = Identity::generate();
    let b = Identity::generate();
    let pa = SecretPassword::new("Local9A".into()).unwrap();
    let pb = SecretPassword::new("Local9B".into()).unwrap();
    let ta = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let tb = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let reserve = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let relay = reserve.local_addr().unwrap();
    drop(reserve);
    let mut signal_config = SignalServerConfig::for_tests();
    if relay_enabled {
        signal_config.relay = Some(crate::relay::server::RelayServerConfig {
            listen: vec![relay],
            ..Default::default()
        });
    }
    let (address, server) = start_local_server_with(signal_config).await;
    let blackhole_a = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let blackhole_b = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let mut ca = auth_config(address);
    ca.remote_auth = Some(RemoteVerifier::create(&pa).unwrap());
    ca.transfer = Some(service_a.clone());
    let mut cb = auth_config(address);
    cb.remote_auth = Some(RemoteVerifier::create(&pb).unwrap());
    cb.transfer = Some(service_b.clone());
    ca.allowed_forward_targets
        .push(crate::node::core::config::AllowedForwardTarget::new(
            "a",
            ta.local_addr().unwrap(),
            vec![b.node_id().to_hex()],
        ));
    cb.allowed_forward_targets
        .push(crate::node::core::config::AllowedForwardTarget::new(
            "b",
            tb.local_addr().unwrap(),
            vec![a.node_id().to_hex()],
        ));
    if relay_enabled {
        for (config, blackhole) in [(&mut ca, &blackhole_a), (&mut cb, &blackhole_b)] {
            config.network.families = crate::net::NetworkFamilies::Ipv4Only;
            config.network.relay_server = Some(relay.to_string());
            config.network.advertise_only = true;
            config.network.advertise = vec![blackhole.local_addr().unwrap()];
        }
    }
    let (ha, mut ea) = spawn(a.clone(), ca).unwrap();
    let (hb, mut eb) = spawn(b.clone(), cb).unwrap();
    let short_a = time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(SessionEvent::ShortIdRegistered(id)) = ea.recv().await {
                break id;
            }
        }
    })
    .await
    .unwrap();
    wait_signal_online(&mut ea).await;
    wait_signal_online(&mut eb).await;
    ha.connect_peer_with_password(b.node_id(), pb).unwrap();
    assert_eq!(
        wait_authorization(&mut ea, b.node_id()).await,
        RemoteAuthorization {
            inbound: crate::node::core::remote_auth::AuthorizationGrant::password(false),
            outbound: crate::node::core::remote_auth::AuthorizationGrant::password(true)
        }
    );
    assert_eq!(
        wait_authorization(&mut eb, a.node_id()).await,
        RemoteAuthorization {
            inbound: crate::node::core::remote_auth::AuthorizationGrant::password(true),
            outbound: crate::node::core::remote_auth::AuthorizationGrant::password(false)
        }
    );
    let conn_a = inspect(&ha).await[&b.node_id()].1.clone();
    let conn_b = inspect(&hb).await[&a.node_id()].1.clone();
    if relay_enabled {
        assert_eq!(conn_b.remote_address(), relay);
    }
    // Reject forged peer requests despite an authenticated, live QUIC transport.
    let (mut send, mut recv) = conn_b.open_bi().await.unwrap();
    let forged_request = protocol::write(
        &mut send,
        &Frame {
            request_id: 0,
            message: Message::TunnelOpen {
                target: ta.local_addr().unwrap(),
            },
        },
    )
    .await;
    // The unauthorized stream can be rejected before the request's final
    // bytes are written. Accept only the exact authorization STOP code;
    // other transport errors still fail this business-boundary test.
    if let Err(error) = forged_request {
        let crate::error::Error::Io(error) = error else {
            panic!("unexpected forged-request error: {error}");
        };
        assert_eq!(
            error
                .get_ref()
                .and_then(|error| error.downcast_ref::<quinn::WriteError>()),
            Some(&quinn::WriteError::Stopped(4u32.into())),
        );
    }
    assert!(
        time::timeout(Duration::from_secs(5), protocol::read(&mut recv))
            .await
            .unwrap()
            .is_err()
    );
    assert!(
        time::timeout(Duration::from_millis(100), ta.accept())
            .await
            .is_err()
    );
    let source_b = root.join("reverse.bin");
    std::fs::write(&source_b, b"reverse").unwrap();
    hb.send_paths(
        a.node_id(),
        vec![source_b.clone(), root.join("must-not-be-read")],
    )
    .unwrap();
    hb.send_file(a.node_id(), source_b.clone()).unwrap();
    hb.start_speed(
        a.node_id(),
        crate::node::core::config::SpeedtestDirection::Upload,
        30,
    )
    .unwrap();
    time::timeout(Duration::from_secs(5), async {
        loop {
            if matches!(
                eb.recv().await,
                Some(SessionEvent::SpeedRequestEnded { .. })
            ) {
                break;
            }
        }
    })
    .await
    .unwrap();
    assert!(
        service_b
            .snapshot()
            .await
            .unwrap()
            .iter()
            .all(|r| r.direction() != TaskDirection::Send)
    );
    // Restored/prepared queue entries also obey the outgoing gate.
    let queued = service_b.select_file(a.node_id(), source_b).await.unwrap();
    time::sleep(Duration::from_millis(150)).await;
    assert_eq!(
        service_b.task(queued.clone()).await.unwrap().state(),
        TaskState::Queued
    );
    assert_eq!(
        std::fs::read_dir(root.join("a-receive")).unwrap().count(),
        0
    );
    let source_a = root.join("forward.bin");
    std::fs::write(&source_a, b"forward").unwrap();
    let (cleanup_reached, cleanup_release) = service_a.gate_test_send_cleanup();
    ha.send_file(b.node_id(), source_a).unwrap();
    time::timeout(Duration::from_secs(10), async {
        while !service_a
            .snapshot()
            .await
            .unwrap()
            .iter()
            .any(|r| r.direction() == TaskDirection::Send && r.state() == TaskState::Completed)
        {
            time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        std::fs::read(root.join("b-receive/forward.bin")).unwrap(),
        b"forward"
    );
    // Completed is durable state, not proof that the stream's cleanup permit
    // has been released. Force that ordering and retain the file/speed gate.
    time::timeout(Duration::from_secs(5), cleanup_reached)
        .await
        .unwrap()
        .unwrap();
    assert!(!service_a.test_speed_idle(b.node_id()));
    assert!(
        service_a
            .start_speed(
                b.node_id(),
                crate::node::core::protocol::SpeedDirection::Upload,
                30
            )
            .await
            .unwrap_err()
            .to_string()
            .contains("文件或测速正在进行")
    );
    cleanup_release.send(()).unwrap();
    time::timeout(Duration::from_secs(5), async {
        while !(service_a.test_speed_idle(b.node_id())
            && service_b.test_speed_idle(a.node_id())
            && service_a.set_test_speed_duration(b.node_id(), Duration::from_millis(100))
            && service_b.set_test_speed_duration(a.node_id(), Duration::from_millis(100)))
        {
            time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    ha.start_speed(
        b.node_id(),
        crate::node::core::config::SpeedtestDirection::Both,
        30,
    )
    .unwrap();
    time::timeout(Duration::from_secs(15), async {
        let mut phases = 0;
        let mut diagnostics = Vec::new();
        while phases < 2 {
            match ea.recv().await.unwrap() {
                SessionEvent::SpeedPhaseCompleted { snapshot, .. } => {
                    assert!(snapshot.bytes > 0);
                    phases += 1;
                }
                SessionEvent::Diagnostic(message) => diagnostics.push(message),
                SessionEvent::SpeedRequestEnded { .. } => {
                    panic!("speed request ended after {phases}/2 phases: {diagnostics:?}")
                }
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    let (mut send_a, mut recv_a) = conn_a.open_bi().await.unwrap();
    protocol::write(
        &mut send_a,
        &Frame {
            request_id: 0,
            message: Message::TunnelOpen {
                target: tb.local_addr().unwrap(),
            },
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        protocol::read(&mut recv_a).await.unwrap().message,
        Message::TunnelReady
    ));
    drop(tb.accept().await.unwrap());
    drop((send_a, recv_a));
    // B separately proves A's password; A proves its current-process B credential again.
    hb.connect_short_id(short_a, pa).unwrap();
    assert_eq!(
        wait_authorization(&mut ea, b.node_id()).await,
        RemoteAuthorization::BOTH
    );
    assert_eq!(
        wait_authorization(&mut eb, a.node_id()).await,
        RemoteAuthorization::BOTH
    );
    assert_ne!(
        inspect(&hb).await[&a.node_id()].1.stable_id(),
        conn_b.stable_id()
    );
    time::timeout(Duration::from_secs(10), async {
        while service_b.task(queued.clone()).await.unwrap().state() != TaskState::Completed {
            time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        std::fs::read(root.join("a-receive/reverse.bin")).unwrap(),
        b"reverse"
    );
    let new_b = inspect(&hb).await[&a.node_id()].1.clone();
    let (mut send, mut recv) = new_b.open_bi().await.unwrap();
    protocol::write(
        &mut send,
        &Frame {
            request_id: 0,
            message: Message::TunnelOpen {
                target: ta.local_addr().unwrap(),
            },
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        protocol::read(&mut recv).await.unwrap().message,
        Message::TunnelReady
    ));
    drop(ta.accept().await.unwrap());
    drop((send, recv));
    ha.shutdown();
    hb.shutdown();
    drop((ha, hb, service_a, service_b));
    server.abort();
    time::timeout(Duration::from_secs(5), async {
        loop {
            if std::fs::remove_dir_all(&root).is_ok() {
                break;
            }
            time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn short_id_connect_authenticates_then_rotation_revokes_and_old_password_fails() {
    let (address, server) = start_local_server().await;
    let a = Identity::generate();
    let b = Identity::generate();
    let (ha, mut ea) = spawn(a.clone(), auth_config(address)).unwrap();
    let short_a = time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(SessionEvent::ShortIdRegistered(id)) = ea.recv().await {
                break id;
            }
        }
    })
    .await
    .unwrap();
    wait_signal_online(&mut ea).await;
    let (hb, mut eb) = spawn(b.clone(), auth_config(address)).unwrap();
    let short_b = time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(SessionEvent::ShortIdRegistered(id)) = eb.recv().await {
                break id;
            }
        }
    })
    .await
    .unwrap();
    wait_signal_online(&mut eb).await;
    assert_ne!(short_a, short_b);
    ha.connect_short_id(short_b, test_password()).unwrap();
    wait_connected(&mut ea, &[b.node_id()]).await;
    wait_connected(&mut eb, &[a.node_id()]).await;
    let old = inspect(&ha).await[&b.node_id()].1.clone();
    let changed = SecretPassword::new("Changed9".into()).unwrap();
    hb.update_remote_auth(RemoteVerifier::create(&changed).unwrap())
        .await
        .unwrap();
    assert!(inspect(&hb).await.is_empty());
    time::timeout(Duration::from_secs(5), old.closed())
        .await
        .unwrap();
    // Wait until the old generation is retired on A; its authorization cannot carry over.
    time::timeout(Duration::from_secs(5), async {
        loop {
            if inspect(&ha).await.is_empty() {
                break;
            }
            time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    while ea.try_recv().is_ok() {}
    while eb.try_recv().is_ok() {}
    ha.connect_peer_with_password(b.node_id(), test_password())
        .unwrap();
    wait_failed(&mut ea).await;
    wait_failed(&mut eb).await;
    assert!(inspect(&ha).await.is_empty());
    while eb.try_recv().is_ok() {}
    ha.connect_peer_with_password(b.node_id(), changed).unwrap();
    wait_connected(&mut ea, &[b.node_id()]).await;
    wait_connected(&mut eb, &[a.node_id()]).await;
    assert_ne!(
        inspect(&ha).await[&b.node_id()].1.stable_id(),
        old.stable_id()
    );
    ha.shutdown();
    hb.shutdown();
    server.abort();
}
#[tokio::test]
async fn signaling_tls_session_reconnect_preserves_transport_and_policy_changes_revoke_it() {
    async fn disconnected(events: &mut mpsc::Receiver<SessionEvent>) {
        time::timeout(Duration::from_secs(15), async {
            loop {
                if matches!(
                    events.recv().await.unwrap(),
                    SessionEvent::Lifecycle(NetworkLifecycle::ReconnectingSignal { .. })
                ) {
                    break;
                }
            }
        })
        .await
        .unwrap();
    }
    let fixture = crate::discovery::signal_tls::tests::Fixture::new(false);
    let listener = Arc::new(TcpListener::bind("127.0.0.1:0").await.unwrap());
    let address = listener.local_addr().unwrap();
    let config = SignalServerConfig {
        tls: Some(fixture.server.clone()),
        ..SignalServerConfig::for_tests()
    };
    let active = listener.clone();
    let first_config = config.clone();
    let server =
        tokio::spawn(async move { run_signal_server_on_borrowed(&active, first_config).await });
    let a = Identity::generate();
    let b = Identity::generate();
    let mut ca = local_config(address);
    ca.signal_tls = Some(fixture.client.clone());
    let cb = ca.clone();
    let (ha, mut ea) = spawn(a.clone(), ca).unwrap();
    let (hb, mut eb) = spawn(b.clone(), cb).unwrap();
    wait_signal_online(&mut ea).await;
    wait_signal_online(&mut eb).await;
    ha.connect_peer(b.node_id()).unwrap();
    wait_connected(&mut ea, &[b.node_id()]).await;
    wait_connected(&mut eb, &[a.node_id()]).await;
    let old = inspect(&ha).await[&b.node_id()].1.clone();
    let binding = ChannelBinding::from_connection(&old).unwrap();
    server.abort();
    let _ = server.await;
    disconnected(&mut ea).await;
    disconnected(&mut eb).await;
    let active = listener.clone();
    let server = tokio::spawn(async move { run_signal_server_on_borrowed(&active, config).await });
    wait_signal_online(&mut ea).await;
    wait_signal_online(&mut eb).await;
    assert_eq!(
        inspect(&ha).await[&b.node_id()].1.stable_id(),
        old.stable_id(),
        "signaling outage retains the live authenticated device transport"
    );
    while ea.try_recv().is_ok() {}
    let wrong = crate::discovery::signal_tls::SignalTlsClient {
        server_name: Some("wrong.invalid".into()),
        ..fixture.client.clone()
    };
    ha.reconfigure_network(address.to_string(), None, Some(wrong))
        .unwrap();
    // Registration cannot publish with the wrong certificate name.
    time::timeout(Duration::from_secs(15), async {
        loop {
            match ea.recv().await.unwrap() {
                SessionEvent::Lifecycle(NetworkLifecycle::SignalOnline)
                | SessionEvent::SignalIdentityRegistered(_) => {
                    panic!("invalid certificate name published signaling success")
                }
                SessionEvent::Lifecycle(NetworkLifecycle::ReconnectingSignal { .. }) => break,
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    assert!(inspect(&ha).await.is_empty());
    assert!(old.close_reason().is_some());
    ha.reconfigure_network(address.to_string(), None, Some(fixture.client.clone()))
        .unwrap();
    wait_signal_online(&mut ea).await;
    ha.connect_peer(b.node_id()).unwrap();
    wait_connected(&mut ea, &[b.node_id()]).await;
    wait_connected(&mut eb, &[a.node_id()]).await;
    assert_ne!(
        ChannelBinding::from_connection(&inspect(&ha).await[&b.node_id()].1).unwrap(),
        binding
    );
    ha.shutdown();
    hb.shutdown();
    drop((ha, hb));
    server.abort();
    let _ = server.await;
}
#[test]
fn auto_resume_requires_network_loss_not_exit_or_protocol_failure() {
    assert!(recoverable_transport_close(
        &quinn::ConnectionError::TimedOut
    ));
    assert!(recoverable_transport_close(&quinn::ConnectionError::Reset));
    assert!(!recoverable_transport_close(
        &quinn::ConnectionError::LocallyClosed
    ));
    assert!(!recoverable_transport_close(
        &quinn::ConnectionError::VersionMismatch
    ));
    assert!(!recoverable_transport_close(
        &quinn::ConnectionError::ApplicationClosed(quinn::ApplicationClose {
            error_code: 0u32.into(),
            reason: b"restart".to_vec().into()
        })
    ));
}
/// Tear down a real QUIC transport after recording the same typed loss
/// evidence as Quinn TimedOut/Reset. This avoids a 30s UDP idle timeout in
/// every platform fixture while retaining the real Session/auth/queue path.
async fn lose_test_transport(
    a: &NodeSessionHandle,
    b: &NodeSessionHandle,
    peer_a: NodeId,
    peer_b: NodeId,
) {
    for (handle, peer) in [(a, peer_b), (b, peer_a)] {
        let (done, wait) = oneshot::channel();
        handle
            .commands
            .send(SessionCommand::TestNetworkLoss(peer, done))
            .await
            .unwrap();
        wait.await.unwrap();
    }
    let connection = inspect(a).await[&peer_b].1.clone();
    connection.close(0u32.into(), b"fixture network loss");
}
#[tokio::test]
async fn auto_resume_real_sessions_restore_same_task_over_fresh_authenticated_transport() {
    use crate::node::core::task_model::TaskState;
    use crate::node::core::task_store::TaskStore;
    use crate::node::core::transfer::TransferService;
    for (sender_enabled, receiver_enabled) in [(true, false), (false, true), (true, true)] {
        let root =
            std::env::temp_dir().join(format!("p2p-auto-resume-{:032x}", rand::random::<u128>()));
        std::fs::create_dir_all(root.join("receive-a")).unwrap();
        std::fs::create_dir_all(root.join("receive-b")).unwrap();
        let sa = TransferService::new(
            TaskStore::open(&root.join("a/tasks.json")).unwrap().0,
            root.join("receive-a"),
        );
        let sb = TransferService::new(
            TaskStore::open(&root.join("b/tasks.json")).unwrap().0,
            root.join("receive-b"),
        );
        sa.set_file_limits(crate::node::core::bandwidth::FileLimits {
            upload_kib: 1024,
            download_kib: 0,
        })
        .unwrap();
        sa.set_auto_resume(sender_enabled);
        sb.set_auto_resume(receiver_enabled);
        let (signal, server) = start_local_server().await;
        let a = Identity::generate();
        let b = Identity::generate();
        let mut ca = local_config(signal);
        ca.transfer = Some(sa.clone());
        let mut cb = local_config(signal);
        cb.transfer = Some(sb.clone());
        let (ha, mut ea) = spawn(a.clone(), ca).unwrap();
        let (hb, mut eb) = spawn(b.clone(), cb).unwrap();
        wait_signal_online(&mut ea).await;
        wait_signal_online(&mut eb).await;
        ha.connect_peer(b.node_id()).unwrap();
        wait_connected(&mut ea, &[b.node_id()]).await;
        // Both directional grants are fresh for each device; auto receive
        // requests require both outgoing RPC and incoming file permission.
        hb.connect_peer(a.node_id()).unwrap();
        time::timeout(Duration::from_secs(10), async {
            while !(sa.test_speed_idle(b.node_id()) && sb.test_speed_idle(a.node_id())) {
                time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let bytes: Vec<u8> = (0..4 * 1024 * 1024).map(|i| (i % 251) as u8).collect();
        let directory = sender_enabled && receiver_enabled;
        let source_dir = root.join("自动恢复目录");
        if directory {
            std::fs::create_dir_all(source_dir.join("空目录")).unwrap();
        }
        let source = if directory {
            source_dir.join("resume.bin")
        } else {
            root.join("resume.bin")
        };
        std::fs::write(&source, &bytes).unwrap();
        let (reached, gate) = oneshot::channel();
        let (_release, wait) = oneshot::channel();
        *sb.checkpoint_gate.lock().unwrap() = Some((reached, wait));
        if directory {
            ha.send_directory(b.node_id(), source_dir).unwrap();
        } else {
            ha.send_file(b.node_id(), source).unwrap();
        }
        time::timeout(Duration::from_secs(10), gate)
            .await
            .unwrap()
            .unwrap();
        let checkpoint_record = sb
            .snapshot()
            .await
            .unwrap()
            .into_iter()
            .find(|r| r.file_details().is_some() && r.state() == TaskState::Transferring)
            .unwrap();
        let durable = tokio::task::spawn_blocking(move || {
            crate::node::core::transfer_files::open_download(&checkpoint_record)
                .unwrap()
                .durable_bitmap()
                .count_set()
        })
        .await
        .unwrap();
        assert!(durable > 0);
        let original = sa.snapshot().await.unwrap();
        let selected_count = original.len();
        assert!(if directory {
            selected_count > 1
        } else {
            selected_count == 1
        });
        let original_file = original
            .iter()
            .find(|r| r.file_details().is_some())
            .unwrap();
        let id = original_file.task_id().clone();
        let destination = root
            .join("receive-b")
            .join(original_file.relative_path().unwrap());
        let (generation, old) = inspect(&ha).await[&b.node_id()].clone();
        let binding = ChannelBinding::from_connection(&old).unwrap();
        lose_test_transport(&ha, &hb, a.node_id(), b.node_id()).await;
        time::timeout(Duration::from_secs(30), async {
            loop {
                let ready = sa
                    .snapshot()
                    .await
                    .unwrap()
                    .iter()
                    .any(|r| r.task_id() == &id && r.state() == TaskState::Completed);
                if ready {
                    break;
                }
                time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!("auto restore failed sender={sender_enabled} receiver={receiver_enabled}")
        });
        let (fresh_generation, fresh) = inspect(&ha).await[&b.node_id()].clone();
        assert_ne!(fresh_generation, generation);
        assert_ne!(fresh.stable_id(), old.stable_id());
        assert_ne!(ChannelBinding::from_connection(&fresh).unwrap(), binding);
        assert_eq!(sa.snapshot().await.unwrap().len(), selected_count);
        assert_eq!(sb.snapshot().await.unwrap().len(), selected_count);
        assert_eq!(std::fs::read(destination).unwrap(), bytes);
        assert!(
            sa.snapshot()
                .await
                .unwrap()
                .iter()
                .all(|r| r.receipt_committed())
        );
        assert!(
            sb.snapshot()
                .await
                .unwrap()
                .iter()
                .all(|r| r.receipt_committed())
        );
        ha.shutdown();
        hb.shutdown();
        drop((ha, hb));
        server.abort();
        let _ = server.await;
        std::fs::remove_dir_all(root).unwrap();
    }
}
async fn start_local_server() -> (SocketAddr, JoinHandle<()>) {
    start_local_server_with(SignalServerConfig::for_tests()).await
}

async fn start_local_server_with(config: SignalServerConfig) -> (SocketAddr, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let _ = run_signal_server_on_with(listener, config).await;
    });
    (address, server)
}

fn local_config(signal_server: SocketAddr) -> NodeSessionConfig {
    NodeSessionConfig {
        signal_server: signal_server.to_string(),
        signal_tls: None,
        remote_auth: Some(test_verifier()),
        trusted_devices: Vec::new(),
        config_path: None,
        test_outgoing_password: Some(test_password()),
        allowed_forward_targets: Vec::new(),
        tunnel_rules: Vec::new(),
        transfer: None,
        network: DesktopNetworkConfig {
            local_port: 0,
            stun_servers: Vec::new(),
            include_loopback: true,
            ..DesktopNetworkConfig::default()
        },
        ..NodeSessionConfig::new(signal_server.to_string())
    }
}

async fn inspect(handle: &NodeSessionHandle) -> HashMap<NodeId, (u64, quinn::Connection)> {
    let (tx, rx) = oneshot::channel();
    handle
        .commands
        .send(SessionCommand::Inspect(tx))
        .await
        .unwrap();
    time::timeout(Duration::from_secs(2), rx)
        .await
        .unwrap()
        .unwrap()
}

async fn wait_signal_online(events: &mut mpsc::Receiver<SessionEvent>) {
    time::timeout(Duration::from_secs(5), async {
        while let Some(event) = events.recv().await {
            if matches!(
                event,
                SessionEvent::Lifecycle(NetworkLifecycle::SignalOnline)
            ) {
                return;
            }
        }
        panic!("session stopped before signal registration");
    })
    .await
    .expect("signal registration should complete");
}

async fn wait_connected(events: &mut mpsc::Receiver<SessionEvent>, expected: &[NodeId]) {
    time::timeout(Duration::from_secs(20), async {
        let mut connected = HashSet::new();
        while connected.len() < expected.len() {
            let Some(event) = events.recv().await else {
                panic!("session stopped before all peers authenticated");
            };
            if let SessionEvent::PeerState {
                state: PeerLifecycle::Failed(ref detail),
                ..
            } = event
            {
                panic!("peer establishment failed: {detail}");
            }
            if let SessionEvent::PeerState {
                peer,
                state: PeerLifecycle::Connected(_),
                ..
            } = event
            {
                connected.insert(peer);
            }
        }
        for peer in expected {
            assert!(
                connected.contains(peer),
                "missing authenticated peer {peer}"
            );
        }
    })
    .await
    .expect("authenticated peer connection should complete");
}

async fn wait_tunnel_state(
    events: &mut mpsc::Receiver<SessionEvent>,
    rule_id: &str,
) -> TunnelRuntimeState {
    time::timeout(Duration::from_secs(8), async {
        while let Some(event) = events.recv().await {
            if let SessionEvent::TunnelState {
                rule_id: current,
                state,
            } = event
                && current == rule_id
                && state != TunnelRuntimeState::Starting
            {
                return state;
            }
        }
        panic!("session stopped before tunnel state was reported");
    })
    .await
    .expect("tunnel startup/stop should report promptly")
}

#[tokio::test]
async fn passive_peer_uses_same_registration_and_third_peer_progresses() {
    let (signal_server, server) = start_local_server().await;
    let identity_a = Identity::generate();
    let identity_b = Identity::generate();
    let identity_c = Identity::generate();
    let (handle_a, mut events_a) = spawn(identity_a.clone(), local_config(signal_server)).unwrap();
    let (handle_b, mut events_b) = spawn(identity_b.clone(), local_config(signal_server)).unwrap();
    let (handle_c, mut events_c) = spawn(identity_c.clone(), local_config(signal_server)).unwrap();

    wait_signal_online(&mut events_a).await;
    wait_signal_online(&mut events_b).await;
    wait_signal_online(&mut events_c).await;

    // A never chooses or enters B. B's lookup must reach the passive session
    // and complete the existing authenticated handshake in both directions.
    handle_b.connect_peer(identity_a.node_id()).unwrap();
    wait_connected(&mut events_a, &[identity_b.node_id()]).await;
    wait_connected(&mut events_b, &[identity_a.node_id()]).await;

    let before_b = inspect(&handle_b).await;
    let original_connection = &before_b[&identity_a.node_id()].1;

    // Keep B<->A alive while C requests B. One registration per identity and
    // the shared endpoint must allow this third peer to progress independently.
    handle_c.connect_peer(identity_b.node_id()).unwrap();
    wait_connected(&mut events_b, &[identity_c.node_id()]).await;
    wait_connected(&mut events_c, &[identity_b.node_id()]).await;
    let after_b = inspect(&handle_b).await;
    assert_eq!(after_b.len(), 2);
    assert_eq!(
        after_b[&identity_a.node_id()].1.stable_id(),
        original_connection.stable_id()
    );
    assert!(
        after_b
            .values()
            .all(|(_, connection)| connection.close_reason().is_none())
    );

    handle_a.shutdown();
    handle_b.shutdown();
    handle_c.shutdown();
    drop((handle_a, handle_b, handle_c));
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn signal_restart_re_registers_the_same_identity_after_bounded_backoff() {
    // Keep one registered listener across epochs. Releasing the port lets
    // parallel fixtures reuse it; cloning/re-registering its socket can spin
    // on accept errors on Windows. Only the server and client tasks restart.
    let reserved = Arc::new(TcpListener::bind("127.0.0.1:0").await.unwrap());
    let signal_server = reserved.local_addr().unwrap();
    let initial = Arc::clone(&reserved);
    let server = tokio::spawn(async move {
        run_signal_server_on_borrowed(
            &initial,
            SignalServerConfig {
                idle_timeout: Duration::from_millis(250),
                ..SignalServerConfig::for_tests()
            },
        )
        .await
        .unwrap();
    });
    let identity = Identity::generate();
    let (handle, mut events) = spawn(identity.clone(), local_config(signal_server)).unwrap();
    wait_signal_online(&mut events).await;
    server.abort();
    let _ = server.await;

    time::timeout(Duration::from_secs(5), async {
        while let Some(event) = events.recv().await {
            if matches!(
                event,
                SessionEvent::Lifecycle(NetworkLifecycle::ReconnectingSignal { .. })
            ) {
                return;
            }
        }
    })
    .await
    .expect("session should detect signaling disconnect and schedule bounded retry");

    let restarted_server = tokio::spawn(async move {
        run_signal_server_on_borrowed(&reserved, SignalServerConfig::for_tests())
            .await
            .unwrap();
    });
    time::timeout(Duration::from_secs(12), async {
        while let Some(event) = events.recv().await {
            if let SessionEvent::SignalIdentityRegistered(node_id) = event {
                assert_eq!(node_id, identity.node_id());
                return;
            }
        }
    })
    .await
    .expect("same identity should register again after the server restarts");

    handle.shutdown();
    drop(handle);
    restarted_server.abort();
    let _ = restarted_server.await;
}

#[tokio::test]
async fn simultaneous_connect_requests_converge_to_one_authenticated_peer() {
    let (signal_server, mut server) = start_local_server().await;
    let identity_a = Identity::generate();
    let identity_b = Identity::generate();
    let (handle_a, mut events_a) = spawn(identity_a.clone(), local_config(signal_server)).unwrap();
    let (handle_b, mut events_b) = spawn(identity_b.clone(), local_config(signal_server)).unwrap();
    wait_signal_online(&mut events_a).await;
    wait_signal_online(&mut events_b).await;

    handle_a.connect_peer(identity_b.node_id()).unwrap();
    handle_a.connect_peer(identity_b.node_id()).unwrap();
    handle_b.connect_peer(identity_a.node_id()).unwrap();
    handle_b.connect_peer(identity_a.node_id()).unwrap();
    wait_connected(&mut events_a, &[identity_b.node_id()]).await;
    wait_connected(&mut events_b, &[identity_a.node_id()]).await;

    let before_a = inspect(&handle_a).await;
    let before_b = inspect(&handle_b).await;
    assert_eq!(before_a.len(), 1);
    assert_eq!(before_b.len(), 1);
    handle_a.connect_peer(identity_b.node_id()).unwrap();
    handle_b.connect_peer(identity_a.node_id()).unwrap();
    let after_a = inspect(&handle_a).await;
    let after_b = inspect(&handle_b).await;
    let conn_a = &before_a[&identity_b.node_id()].1;
    let conn_b = &before_b[&identity_a.node_id()].1;
    assert_eq!(
        conn_a.stable_id(),
        after_a[&identity_b.node_id()].1.stable_id()
    );
    assert_eq!(
        conn_b.stable_id(),
        after_b[&identity_a.node_id()].1.stable_id()
    );
    server.abort();
    let _ = (&mut server).await;
    time::timeout(Duration::from_secs(3), async {
        while let Some(event) = events_a.recv().await {
            if matches!(
                event,
                SessionEvent::Lifecycle(NetworkLifecycle::ReconnectingSignal { .. })
            ) {
                return;
            }
        }
        panic!("session stopped during signaling outage");
    })
    .await
    .unwrap();
    let payload = b"authenticated connection survives signaling outage";
    time::timeout(Duration::from_secs(3), async {
        let send = async {
            let mut stream = conn_a.open_uni().await.unwrap();
            stream.write_all(payload).await.unwrap();
            stream.finish().unwrap();
            stream.stopped().await.unwrap();
        };
        let receive = async {
            let mut stream = conn_b.accept_uni().await.unwrap();
            assert_eq!(stream.read_to_end(1024).await.unwrap(), payload);
        };
        tokio::join!(send, receive);
    })
    .await
    .unwrap();
    assert!(conn_a.close_reason().is_none());
    assert!(conn_b.close_reason().is_none());
    handle_a.shutdown();
    handle_b.shutdown();
    drop((handle_a, handle_b));
}

#[tokio::test]
async fn unexpected_authenticated_target_fails_before_connected() {
    let local_identity = Identity::generate();
    let remote_identity = Identity::generate();
    let unexpected_target = Identity::generate().node_id();
    assert_ne!(unexpected_target, remote_identity.node_id());
    let server = crate::transport::quic::server_endpoint("127.0.0.1:0".parse().unwrap()).unwrap();
    let address = server.local_addr().unwrap();
    let server_task = tokio::spawn(async move {
        let incoming = server.accept().await.expect("QUIC should arrive");
        let connection = incoming.await.expect("transport handshake should pass");
        let binding = ChannelBinding::from_connection(&connection).unwrap();
        let (mut send, mut recv) = connection.accept_bi().await.unwrap();
        handshake_responder(&mut send, &mut recv, &remote_identity, &binding)
            .await
            .unwrap();
        let _ = send.finish();
        connection.closed().await;
        server.wait_idle().await;
    });
    let client = crate::transport::quic::client_endpoint("127.0.0.1:0".parse().unwrap()).unwrap();

    let result = authenticate_outgoing(&client, address, &local_identity, unexpected_target).await;
    assert!(result.unwrap_err().contains("对端身份不符"));
    client.close(0u32.into(), b"test complete");
    client.wait_idle().await;
    server_task.await.unwrap();
}

#[tokio::test]
async fn authenticated_legacy_peer_is_rejected_by_desktop_negotiation() {
    time::timeout(Duration::from_secs(10), async {
        let local = Identity::generate();
        let remote = Identity::generate();
        let expected = remote.node_id();
        let server =
            crate::transport::quic::server_endpoint("127.0.0.1:0".parse().unwrap()).unwrap();
        let address = server.local_addr().unwrap();
        let server_task = tokio::spawn(async move {
            let connection = server.accept().await.unwrap().await.unwrap();
            let binding = ChannelBinding::from_connection(&connection).unwrap();
            let (mut send, mut recv) = connection.accept_bi().await.unwrap();
            handshake_responder(&mut send, &mut recv, &remote, &binding)
                .await
                .unwrap();
            send.finish().unwrap();
            let (send, mut recv) = connection.accept_bi().await.unwrap();
            assert!(crate::protocol::frame::read_frame(&mut recv).await.is_err());
            drop(send);
            connection.closed().await;
            server.wait_idle().await;
        });
        let client =
            crate::transport::quic::client_endpoint("127.0.0.1:0".parse().unwrap()).unwrap();
        let connection = authenticate_outgoing(&client, address, &local, expected)
            .await
            .unwrap();
        let error = crate::node::core::protocol::negotiate(&connection, true)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("桌面版本或能力不兼容"));
        assert!(connection.close_reason().is_some());
        client.close(0u32.into(), b"test complete");
        client.wait_idle().await;
        server_task.await.unwrap();
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn late_probe_registration_receives_reply_at_validated_actual_source() {
    let (endpoint, socket) = crate::transport::quic::endpoint_from_socket_with_punch_dispatcher(
        std::net::UdpSocket::bind("127.0.0.1:0").unwrap(),
    )
    .unwrap();
    let remote = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let token = PunchToken::random();
    let mut probes = socket.register_probe(&token).unwrap();
    let remote_addr = remote.local_addr().unwrap();
    let local_addr = endpoint.local_addr().unwrap();
    let exchange = async {
        let mut packet = [0; 256];
        // Drop the first outgoing probe, as when remote registration is late.
        remote.recv_from(&mut packet).await.unwrap();
        remote
            .send_to(&crate::nat::punch::probe_packet(&token), local_addr)
            .await
            .unwrap();
        let (len, source) = remote.recv_from(&mut packet).await.unwrap();
        assert_eq!(source, local_addr);
        assert!(crate::nat::punch::is_probe_with_token(
            &packet[..len],
            &token
        ));
    };
    time::timeout(Duration::from_secs(2), async {
        let candidates = [remote_addr];
        let (source, ()) = tokio::join!(
            punch_candidates(&socket, &mut probes, &candidates, &token),
            exchange
        );
        assert_eq!(source.unwrap(), remote_addr);
    })
    .await
    .expect("validated probe must receive a final reply even after first packet loss");
    endpoint.close(0u32.into(), b"test complete");
    endpoint.wait_idle().await;
}

#[tokio::test]
async fn reconfiguration_cancels_stalled_registration_and_uses_new_server() {
    let stalled = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let stalled_address = stalled.local_addr().unwrap();
    let (new_address, server) = start_local_server().await;
    let (handle, mut events) = spawn(Identity::generate(), local_config(stalled_address)).unwrap();
    let (mut old_stream, _) = time::timeout(Duration::from_secs(3), stalled.accept())
        .await
        .unwrap()
        .unwrap();
    // TCP accept alone is not a registration barrier: wait for Hello before
    // canceling, so this test really exercises a stalled registration.
    let mut hello_prefix = [0u8; 1];
    time::timeout(
        Duration::from_secs(2),
        tokio::io::AsyncReadExt::read_exact(&mut old_stream, &mut hello_prefix),
    )
    .await
    .unwrap()
    .unwrap();
    handle
        .reconfigure_network(new_address.to_string(), None, None)
        .unwrap();
    wait_signal_online(&mut events).await;
    // Drain the original Hello; canceled registration must then close TCP.
    let mut bytes = Vec::new();
    time::timeout(
        Duration::from_secs(2),
        tokio::io::AsyncReadExt::read_to_end(&mut old_stream, &mut bytes),
    )
    .await
    .expect("superseded registration must release its socket")
    .unwrap();
    assert!(
        !bytes.is_empty(),
        "remaining Hello frame must be drained before EOF"
    );
    handle.shutdown();
    drop(handle);
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn shutdown_joins_runtime_even_with_full_ui_event_queue() {
    let (address, server) = start_local_server().await;
    let (handle, mut events) = spawn(Identity::generate(), local_config(address)).unwrap();
    wait_signal_online(&mut events).await;
    let peer = Identity::generate().node_id();
    // Stop consuming UI events and fill both bounded channels. Shutdown has
    // its own watch channel and cannot get stuck behind these messages.
    for _ in 0..EVENT_CAPACITY + COMMAND_CAPACITY {
        let _ = handle.connect_peer(peer);
        tokio::task::yield_now().await;
    }
    handle.shutdown();
    let lifetime = Arc::clone(&handle.lifetime);
    drop(handle);
    tokio::task::spawn_blocking(move || {
        lifetime
            .done
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(2))
            .expect("network runtime must finish despite UI backpressure");
        let thread = lifetime.thread.lock().unwrap().take().unwrap();
        thread.join().unwrap();
    })
    .await
    .unwrap();
    server.abort();
    let _ = server.await;
}

#[tokio::test]
async fn closing_ui_event_receiver_runs_session_shutdown() {
    let (address, server) = start_local_server().await;
    let (handle, mut events) = spawn(Identity::generate(), local_config(address)).unwrap();
    wait_signal_online(&mut events).await;
    drop(events);
    time::timeout(Duration::from_secs(3), async {
        while handle.is_running() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("closing the UI event receiver must join the session runtime");
    drop(handle);
    server.abort();
    let _ = server.await;
}

#[test]
fn stale_signal_connect_result_is_fenced_by_configuration_generation() {
    let old_generation = 7u64;
    let new_generation = old_generation.wrapping_add(1);
    assert!(is_current_signal_generation(old_generation, old_generation));
    assert!(!is_current_signal_generation(
        new_generation,
        old_generation
    ));
}

async fn wait_specific_tunnel_state(
    events: &mut mpsc::Receiver<SessionEvent>,
    id: &str,
    wanted: TunnelRuntimeState,
) {
    let expected = format!("expected {wanted:?} for {id}");
    // Reconnect now includes an Argon2id job; use the healthy peer establishment
    // budget, and still fail immediately on a terminal protocol/runtime error.
    time::timeout(Duration::from_secs(20), async {
        while let Some(event) = events.recv().await {
            if wanted == TunnelRuntimeState::Running {
                if let SessionEvent::PeerState {
                    state: PeerLifecycle::Failed(ref detail),
                    ..
                } = event
                {
                    panic!("tunnel peer failed while waiting for Running: {detail}");
                }
                if let SessionEvent::TunnelState {
                    ref rule_id,
                    state: TunnelRuntimeState::Error(ref detail),
                } = event
                    && rule_id == id
                {
                    panic!("tunnel failed while waiting for Running: {detail}");
                }
            }
            if let SessionEvent::TunnelState { rule_id, state } = event
                && rule_id == id
                && state == wanted
            {
                return;
            }
        }
        panic!("session ended before expected tunnel state");
    })
    .await
    .expect(&expected);
}

#[tokio::test]
async fn offline_rule_waits_without_a_listener_and_reports_lookup_failure() {
    let (signal, server) = start_local_server().await;
    let probe = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = probe.local_addr().unwrap().port();
    drop(probe);
    let mut rule = crate::node::core::config::TunnelRule::new(
        "offline",
        Identity::generate().node_id().to_hex(),
        port,
        "127.0.0.1:22".parse().unwrap(),
    );
    rule.auto_start = true;
    let mut config = local_config(signal);
    config.tunnel_rules.push(rule.clone());
    let (handle, mut events) = spawn(Identity::generate(), config).unwrap();
    wait_specific_tunnel_state(&mut events, &rule.id, TunnelRuntimeState::Starting).await;
    wait_specific_tunnel_state(
        &mut events,
        &rule.id,
        TunnelRuntimeState::WaitingAuthorization,
    )
    .await;
    assert!(tokio::net::TcpStream::connect(rule.listen).await.is_err());
    time::timeout(Duration::from_secs(25), async {
        while let Some(event) = events.recv().await {
            if let SessionEvent::TunnelState { rule_id, state } = event
                && rule_id == rule.id
            {
                assert_ne!(
                    state,
                    TunnelRuntimeState::Running,
                    "offline listener must never report Running"
                );
                if let TunnelRuntimeState::Error(detail) = state {
                    assert!(
                        detail.contains("离线") || detail.contains("超时"),
                        "{detail}"
                    );
                    return;
                }
            }
        }
        panic!("offline lookup must end with Error");
    })
    .await
    .unwrap();
    assert!(tokio::net::TcpStream::connect(rule.listen).await.is_err());
    drop(handle);
    server.abort();
}

#[tokio::test]
async fn authenticated_transfer_dispatch_checks_peer_and_live_target_revocation() {
    use crate::node::core::config::AllowedForwardTarget;
    use crate::node::core::protocol::{self, Frame, Message};
    use crate::node::core::task_store::TaskStore;
    use crate::node::core::transfer::TransferService;
    let root = std::env::temp_dir().join(format!("p2p-tunnel-auth-{}", rand::random::<u128>()));
    std::fs::create_dir_all(root.join("receive")).unwrap();
    let (store, _) = TaskStore::open(&root.join("state/tasks.json")).unwrap();
    let (signal, server) = start_local_server().await;
    let owner = Identity::generate();
    let a = Identity::generate();
    let b = Identity::generate();
    let echo = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let target = echo.local_addr().unwrap();
    let echo_task = tokio::spawn(async move {
        while let Ok((mut tcp, _)) = echo.accept().await {
            tokio::spawn(async move {
                let (mut r, mut w) = tcp.split();
                let _ = tokio::io::copy(&mut r, &mut w).await;
            });
        }
    });
    let grant = AllowedForwardTarget::new("echo", target, vec![a.node_id().to_hex()]);
    let mut config = local_config(signal);
    config.transfer = Some(TransferService::new(store, root.join("receive")));
    config.allowed_forward_targets = vec![grant.clone()];
    let (owner_handle, mut owner_events) = spawn(owner.clone(), config).unwrap();
    let (a_handle, mut a_events) = spawn(a.clone(), local_config(signal)).unwrap();
    let (b_handle, mut b_events) = spawn(b.clone(), local_config(signal)).unwrap();
    wait_signal_online(&mut owner_events).await;
    wait_signal_online(&mut a_events).await;
    wait_signal_online(&mut b_events).await;
    a_handle.connect_peer(owner.node_id()).unwrap();
    b_handle.connect_peer(owner.node_id()).unwrap();
    wait_connected(&mut a_events, &[owner.node_id()]).await;
    wait_connected(&mut b_events, &[owner.node_id()]).await;
    let a_connection = inspect(&a_handle).await.remove(&owner.node_id()).unwrap().1;
    let b_connection = inspect(&b_handle).await.remove(&owner.node_id()).unwrap().1;
    async fn open(
        connection: &quinn::Connection,
        target: SocketAddr,
    ) -> (quinn::SendStream, quinn::RecvStream, Message) {
        let (mut send, mut recv) = connection.open_bi().await.unwrap();
        protocol::write(
            &mut send,
            &Frame {
                request_id: 0,
                message: Message::TunnelOpen { target },
            },
        )
        .await
        .unwrap();
        let message = time::timeout(Duration::from_secs(2), protocol::read(&mut recv))
            .await
            .unwrap()
            .unwrap()
            .message;
        (send, recv, message)
    }
    let (mut established_send, mut established_recv, message) = open(&a_connection, target).await;
    assert_eq!(message, Message::TunnelReady);
    assert!(matches!(
        open(&b_connection, target).await.2,
        Message::TunnelError { .. }
    ));
    let mut disabled = grant.clone();
    disabled.enabled = false;
    owner_handle.restrict_forward_targets(&[disabled]);
    assert!(matches!(
        open(&a_connection, target).await.2,
        Message::TunnelError { .. }
    ));
    // Revocation does not kill already established streams.
    established_send.write_all(b"kept").await.unwrap();
    let mut echoed = [0; 4];
    established_recv.read_exact(&mut echoed).await.unwrap();
    assert_eq!(&echoed, b"kept");
    owner_handle
        .update_tunnel_settings(vec![grant.clone()], Vec::new())
        .unwrap();
    assert_eq!(open(&a_connection, target).await.2, Message::TunnelReady);
    owner_handle.restrict_forward_targets(&[]);
    let mut unsaved =
        crate::node::core::config::SettingsDraft::defaults(Some(root.join("receive")));
    unsaved.signal_host = "127.0.0.1".into();
    unsaved.signal_port = signal.port().to_string();
    unsaved.allowed_forward_targets = vec![grant.clone()];
    assert!(unsaved.save_atomic(&root.join("receive")).is_err());
    assert!(matches!(
        open(&a_connection, target).await.2,
        Message::TunnelError { .. }
    ));
    // Unsaved re-enable cannot restore grants revoked from the applied configuration.
    owner_handle.restrict_forward_targets(&[grant]);
    assert!(matches!(
        open(&a_connection, target).await.2,
        Message::TunnelError { .. }
    ));
    drop((a_handle, b_handle, owner_handle));
    echo_task.abort();
    server.abort();
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn auto_started_bound_peer_tunnel_is_independent_and_reports_real_errors() {
    let (signal, server) = start_local_server().await;
    let identity_a = Identity::generate();
    let identity_b = Identity::generate();
    let echo = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let target = echo.local_addr().unwrap();
    let echo_task = tokio::spawn(async move {
        while let Ok((mut stream, _)) = echo.accept().await {
            tokio::spawn(async move {
                let (mut read, mut write) = stream.split();
                let _ = tokio::io::copy(&mut read, &mut write).await;
            });
        }
    });
    let listener_probe = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let listen_port = listener_probe.local_addr().unwrap().port();
    drop(listener_probe);
    let occupied_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let occupied = occupied_listener.local_addr().unwrap();

    let mut auto_rule = crate::node::core::config::TunnelRule::new(
        "echo",
        identity_b.node_id().to_hex(),
        listen_port,
        target,
    );
    auto_rule.auto_start = true;
    let conflict_rule = crate::node::core::config::TunnelRule {
        id: "occupied-port".into(),
        name: "占用端口".into(),
        peer_node_id: identity_b.node_id().to_hex(),
        listen: occupied,
        target,
        enabled: true,
        auto_start: true,
    };

    let mut config_a = local_config(signal);
    config_a.tunnel_rules = vec![auto_rule.clone(), conflict_rule.clone()];
    let mut config_b = local_config(signal);
    config_b.allowed_forward_targets = vec![crate::node::core::config::AllowedForwardTarget::new(
        "echo",
        target,
        vec![identity_a.node_id().to_hex()],
    )];
    let (handle_a, mut events_a) = spawn(identity_a.clone(), config_a).unwrap();
    let (handle_b, mut events_b) = spawn(identity_b.clone(), config_b).unwrap();

    let conflict_detail = time::timeout(Duration::from_secs(20), async {
        let mut conflict = None;
        let mut running = false;
        while let Some(event) = events_a.recv().await {
            if let SessionEvent::TunnelState { rule_id, state } = event {
                if rule_id == conflict_rule.id {
                    if let TunnelRuntimeState::Error(detail) = state {
                        conflict = Some(detail);
                    }
                } else if rule_id == auto_rule.id && state == TunnelRuntimeState::Running {
                    running = true;
                }
                if running && let Some(detail) = conflict.take() {
                    return detail;
                }
            }
        }
        panic!("both rules must settle independently");
    })
    .await
    .unwrap();
    assert!(conflict_detail.contains("监听"), "{conflict_detail}");
    assert!(
        conflict_detail.contains("Address already in use") || conflict_detail.contains("10048"),
        "{conflict_detail}"
    );
    wait_connected(&mut events_b, &[identity_a.node_id()]).await;

    let mut local = tokio::net::TcpStream::connect(auto_rule.listen)
        .await
        .unwrap();
    local.write_all(b"bound to configured peer").await.unwrap();
    let mut echoed = [0; 24];
    local.read_exact(&mut echoed).await.unwrap();
    assert_eq!(&echoed, b"bound to configured peer");
    local.shutdown().await.unwrap();
    let mut tail = Vec::new();
    local.read_to_end(&mut tail).await.unwrap();

    inspect(&handle_a)
        .await
        .get(&identity_b.node_id())
        .unwrap()
        .1
        .close(0u32.into(), b"test reconnect");
    wait_specific_tunnel_state(&mut events_a, &auto_rule.id, TunnelRuntimeState::Starting).await;
    wait_specific_tunnel_state(&mut events_a, &auto_rule.id, TunnelRuntimeState::Running).await;

    handle_b
        .update_tunnel_settings(Vec::new(), Vec::new())
        .unwrap();
    let mut denied = tokio::net::TcpStream::connect(auto_rule.listen)
        .await
        .unwrap();
    denied.write_all(b"no longer authorized").await.unwrap();
    let mut discarded = Vec::new();
    let _ = denied.read_to_end(&mut discarded).await;
    assert!(
        discarded.is_empty(),
        "unauthorized target must not return data"
    );
    time::timeout(Duration::from_secs(3), async {
        while let Some(event) = events_a.recv().await {
            if let SessionEvent::TunnelError { rule_id, detail } = event
                && rule_id == auto_rule.id
            {
                assert!(detail.contains("不在允许转发的列表里"), "{detail}");
                return;
            }
        }
        panic!("unauthorized target should produce a detailed tunnel error");
    })
    .await
    .unwrap();

    assert!(matches!(
        handle_a.start_tunnel_rule(auto_rule.id.clone()),
        Ok(())
    ));
    assert_eq!(
        auto_rule.peer_node_id,
        identity_b.node_id().to_hex(),
        "the runtime uses the rule's peer binding"
    );
    handle_a.stop_tunnel_rule(auto_rule.id.clone()).unwrap();
    assert_eq!(
        wait_tunnel_state(&mut events_a, &auto_rule.id).await,
        TunnelRuntimeState::Stopped
    );

    assert!(
        tokio::net::TcpStream::connect(auto_rule.listen)
            .await
            .is_err()
    );
    for delete in [false, true] {
        handle_a
            .update_tunnel_settings(Vec::new(), vec![auto_rule.clone()])
            .unwrap();
        wait_specific_tunnel_state(&mut events_a, &auto_rule.id, TunnelRuntimeState::Running).await;
        time::timeout(
            Duration::from_secs(2),
            handle_a.revoke_tunnel_rule(auto_rule.id.clone(), delete),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(
            tokio::net::TcpStream::connect(auto_rule.listen)
                .await
                .is_err(),
            "acknowledged revoke must release listener"
        );
        let rebound = TcpListener::bind(auto_rule.listen)
            .await
            .expect("local port must be reusable");
        drop(rebound);
        wait_specific_tunnel_state(&mut events_a, &auto_rule.id, TunnelRuntimeState::Stopped).await;
    }

    handle_a.shutdown();
    handle_b.shutdown();
    drop((handle_a, handle_b));
    echo_task.abort();
    server.abort();
    let _ = server.await;
}
#[tokio::test]
async fn native_drop_batch_command_sends_mixed_selection_to_the_bound_peer() {
    use crate::node::core::task_model::TaskState;
    use crate::node::core::task_store::TaskStore;
    use crate::node::core::transfer::TransferService;
    let root =
        std::env::temp_dir().join(format!("p2p-session-transfer-{}", rand::random::<u128>()));
    std::fs::create_dir(&root).unwrap();
    std::fs::create_dir(root.join("a-receive")).unwrap();
    std::fs::create_dir(root.join("b-receive")).unwrap();
    let (a_store, _) = TaskStore::open(&root.join("a-state/tasks.json")).unwrap();
    let (b_store, _) = TaskStore::open(&root.join("b-state/tasks.json")).unwrap();
    let a = TransferService::new(a_store, root.join("a-receive"));
    let b = TransferService::new(b_store, root.join("b-receive"));
    let (address, server) = start_local_server().await;
    let ia = Identity::generate();
    let ib = Identity::generate();
    let mut ca = local_config(address);
    ca.transfer = Some(a.clone());
    let mut cb = local_config(address);
    cb.transfer = Some(b.clone());
    let (ha, mut ea) = spawn(ia.clone(), ca).unwrap();
    let (hb, mut eb) = spawn(ib.clone(), cb).unwrap();
    wait_signal_online(&mut ea).await;
    wait_signal_online(&mut eb).await;
    ha.connect_peer(ib.node_id()).unwrap();
    wait_connected(&mut ea, &[ib.node_id()]).await;
    wait_connected(&mut eb, &[ia.node_id()]).await;
    let bytes = vec![55; 1024 * 1024];
    let source = root.join("selected.bin");
    std::fs::write(&source, &bytes).unwrap();
    let directory = root.join("目录");
    std::fs::create_dir_all(directory.join("空目录")).unwrap();
    std::fs::write(directory.join("子.bin"), b"directory child").unwrap();
    ha.send_paths(
        ib.node_id(),
        vec![
            source.clone(),
            source,
            directory.join("子.bin"),
            directory.join("空目录"),
            directory,
        ],
    )
    .unwrap();
    time::timeout(Duration::from_secs(10), async {
        let mut changed = a.subscribe();
        loop {
            let tasks = a.snapshot().await.unwrap();
            if tasks.len() == 4
                && tasks
                    .iter()
                    .all(|task| task.state() == TaskState::Completed)
            {
                break;
            }
            changed.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    assert_eq!(b.snapshot().await.unwrap()[0].state(), TaskState::Completed);
    assert_eq!(
        std::fs::read(root.join("b-receive/selected.bin")).unwrap(),
        bytes
    );
    assert_eq!(b.snapshot().await.unwrap().len(), 4);
    assert!(
        b.snapshot()
            .await
            .unwrap()
            .iter()
            .all(|task| task.state() == TaskState::Completed)
    );
    assert_eq!(
        std::fs::read(root.join("b-receive/目录/子.bin")).unwrap(),
        b"directory child"
    );
    assert!(root.join("b-receive/目录/空目录").is_dir());
    tokio::task::spawn_blocking(move || {
        ha.shutdown_and_wait();
        hb.shutdown_and_wait();
    })
    .await
    .unwrap();
    server.abort();
    let _ = server.await;
    drop((a, b));
    std::fs::remove_dir_all(root).unwrap();
}
#[tokio::test]
async fn session_command_sends_to_passive_peer_using_owned_transfer_service() {
    use crate::node::core::task_model::TaskState;
    use crate::node::core::task_store::TaskStore;
    use crate::node::core::transfer::TransferService;
    let root =
        std::env::temp_dir().join(format!("p2p-session-transfer-{}", rand::random::<u128>()));
    std::fs::create_dir(&root).unwrap();
    std::fs::create_dir(root.join("a-receive")).unwrap();
    std::fs::create_dir(root.join("b-receive")).unwrap();
    let (a_store, _) = TaskStore::open(&root.join("a-state/tasks.json")).unwrap();
    let (b_store, _) = TaskStore::open(&root.join("b-state/tasks.json")).unwrap();
    let a = TransferService::new(a_store, root.join("a-receive"));
    let b = TransferService::new(b_store, root.join("b-receive"));
    let (address, server) = start_local_server().await;
    let ia = Identity::generate();
    let ib = Identity::generate();
    let mut ca = local_config(address);
    ca.transfer = Some(a.clone());
    let mut cb = local_config(address);
    cb.transfer = Some(b.clone());
    let (ha, mut ea) = spawn(ia.clone(), ca).unwrap();
    let (hb, mut eb) = spawn(ib.clone(), cb).unwrap();
    wait_signal_online(&mut ea).await;
    wait_signal_online(&mut eb).await;
    ha.connect_peer(ib.node_id()).unwrap();
    wait_connected(&mut ea, &[ib.node_id()]).await;
    wait_connected(&mut eb, &[ia.node_id()]).await;
    let bytes = vec![55; 1024 * 1024];
    let source = root.join("selected.bin");
    std::fs::write(&source, &bytes).unwrap();
    ha.send_file(ib.node_id(), source).unwrap();
    time::timeout(Duration::from_secs(10), async {
        let mut changed = a.subscribe();
        loop {
            let tasks = a.snapshot().await.unwrap();
            if tasks.len() == 1 && tasks[0].state() == TaskState::Completed {
                break;
            }
            changed.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    assert_eq!(b.snapshot().await.unwrap()[0].state(), TaskState::Completed);
    assert_eq!(
        std::fs::read(root.join("b-receive/selected.bin")).unwrap(),
        bytes
    );
    ha.shutdown();
    hb.shutdown();
    drop((ha, hb));
    server.abort();
    let _ = server.await;
    drop((a, b));
    std::fs::remove_dir_all(root).unwrap();
}
#[tokio::test]
async fn explicit_mapping_refresh_preserves_unchanged_authenticated_connection() {
    let (addr, server) = start_local_server().await;
    let ia = Identity::generate();
    let ib = Identity::generate();
    let (a, mut ea) = spawn(ia.clone(), local_config(addr)).unwrap();
    let (b, mut eb) = spawn(ib.clone(), local_config(addr)).unwrap();
    wait_signal_online(&mut ea).await;
    wait_signal_online(&mut eb).await;
    a.connect_peer(ib.node_id()).unwrap();
    wait_connected(&mut ea, &[ib.node_id()]).await;
    wait_connected(&mut eb, &[ia.node_id()]).await;
    let ca = inspect(&a).await[&ib.node_id()].1.clone();
    let cb = inspect(&b).await[&ia.node_id()].1.clone();
    while ea.try_recv().is_ok() {}
    while eb.try_recv().is_ok() {}
    a.connect_peer(ib.node_id()).unwrap();
    for events in [&mut ea, &mut eb] {
        time::timeout(Duration::from_secs(3),async {
                loop { if matches!(events.recv().await.unwrap(),SessionEvent::Diagnostic(s) if s=="地址核对完毕，现有认证连接继续使用") { break; } }
            }).await.expect("must observe processed unchanged candidate response, not just queued Connect");
    }
    assert_eq!(
        inspect(&a).await[&ib.node_id()].1.stable_id(),
        ca.stable_id()
    );
    assert_eq!(
        inspect(&b).await[&ia.node_id()].1.stable_id(),
        cb.stable_id()
    );
    let send = async {
        let mut s = ca.open_uni().await.unwrap();
        s.write_all(b"unchanged mapping live").await.unwrap();
        s.finish().unwrap();
        s.stopped().await.unwrap();
    };
    let receive = async {
        let mut r = cb.accept_uni().await.unwrap();
        assert_eq!(r.read_to_end(128).await.unwrap(), b"unchanged mapping live");
    };
    time::timeout(Duration::from_secs(3), async {
        tokio::join!(send, receive);
    })
    .await
    .unwrap();
    assert!(ca.close_reason().is_none());
    assert!(cb.close_reason().is_none());
    a.shutdown();
    b.shutdown();
    drop((a, b));
    server.abort();
    let _ = server.await;
}
fn security_config(
    path: &std::path::Path,
    devices: Vec<crate::node::core::trusted_devices::TrustedDevice>,
) {
    crate::node::core::config::DesktopConfig::save_remote_auth(path, test_verifier()).unwrap();
    crate::node::core::config::DesktopConfig::save_trusted_devices(path, devices).unwrap();
}
async fn wait_grants(
    events: &mut mpsc::Receiver<SessionEvent>,
    peer: NodeId,
    expected: RemoteAuthorization,
) {
    time::timeout(Duration::from_secs(20), async {
        loop {
            if let SessionEvent::PeerState {
                peer: id,
                state: PeerLifecycle::Connected(auth),
                ..
            } = events
                .recv()
                .await
                .expect("session closed before expected grants")
                && id == peer
                && auth == expected
            {
                return;
            }
        }
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn explicit_trust_requires_inbound_password_and_failed_save_cannot_grant() {
    use crate::node::core::config::DesktopConfig;
    use crate::node::core::remote_auth::AuthorizationGrant;
    let root = std::env::temp_dir().join(format!(
        "p2p-explicit-trust-{:032x}",
        rand::random::<u128>()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("settings.json");
    let client_path = root.join("client.json");
    security_config(&path, vec![]);
    security_config(&client_path, vec![]);
    let (signal, server) = start_local_server().await;
    let a = Identity::generate();
    let b = Identity::generate();
    let mut cb = auth_config(signal);
    cb.config_path = Some(path.clone());
    let mut ca = auth_config(signal);
    ca.config_path = Some(client_path.clone());
    let (ha, mut ea) = spawn(a.clone(), ca).unwrap();
    let (hb, mut eb) = spawn(b.clone(), cb).unwrap();
    wait_signal_online(&mut ea).await;
    wait_signal_online(&mut eb).await;
    ha.connect_peer_with_password(b.node_id(), test_password())
        .unwrap();
    let expected_a = RemoteAuthorization {
        inbound: AuthorizationGrant::default(),
        outbound: AuthorizationGrant::password(true),
    };
    let expected_b = RemoteAuthorization {
        inbound: AuthorizationGrant::password(true),
        outbound: AuthorizationGrant::default(),
    };
    wait_grants(&mut ea, b.node_id(), expected_a).await;
    wait_grants(&mut eb, a.node_id(), expected_b).await;
    let client_generation = inspect(&ha).await[&b.node_id()].0;
    let error = ha
        .change_trusted_device(TrustedDeviceChange::Trust {
            peer: b.node_id(),
            generation: client_generation,
            display_name: "家里 Ubuntu".into(),
        })
        .await
        .unwrap_err();
    assert!(error.contains("本机密码"));
    assert!(
        crate::node::core::config::SettingsDraft::from_config(
            DesktopConfig::load(&client_path).unwrap().unwrap()
        )
        .trusted_devices
        .is_empty()
    );
    assert!(
        crate::node::core::config::SettingsDraft::from_config(
            DesktopConfig::load(&path).unwrap().unwrap()
        )
        .trusted_devices
        .is_empty()
    );
    let (generation, connection) = inspect(&hb).await[&a.node_id()].clone();
    assert!(
        hb.change_trusted_device(TrustedDeviceChange::Trust {
            peer: a.node_id(),
            generation: generation.wrapping_add(1),
            display_name: "stale".into()
        })
        .await
        .is_err()
    );
    let backup = root.join("saved.json");
    std::fs::rename(&path, &backup).unwrap();
    std::fs::create_dir(&path).unwrap();
    assert!(
        hb.change_trusted_device(TrustedDeviceChange::Trust {
            peer: a.node_id(),
            generation,
            display_name: "device".into()
        })
        .await
        .is_err()
    );
    std::fs::remove_dir(&path).unwrap();
    std::fs::rename(&backup, &path).unwrap();
    assert!(
        crate::node::core::config::SettingsDraft::from_config(
            DesktopConfig::load(&path).unwrap().unwrap()
        )
        .trusted_devices
        .is_empty()
    );
    let devices = change_trusted_device(
        Some(&hb),
        &path,
        TrustedDeviceChange::Trust {
            peer: a.node_id(),
            generation,
            display_name: "家里 Ubuntu".into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(devices[0].node_id, a.node_id().to_hex());
    let mixed_b = RemoteAuthorization {
        inbound: AuthorizationGrant {
            password: true,
            trusted_device: true,
        },
        outbound: AuthorizationGrant::default(),
    };
    let mixed_a = RemoteAuthorization {
        inbound: AuthorizationGrant::default(),
        outbound: mixed_b.inbound,
    };
    wait_grants(&mut ea, b.node_id(), mixed_a).await;
    wait_grants(&mut eb, a.node_id(), mixed_b).await;
    hb.change_trusted_device(TrustedDeviceChange::Rename {
        peer: a.node_id(),
        display_name: "renamed".into(),
    })
    .await
    .unwrap();
    assert_eq!(
        inspect(&hb).await[&a.node_id()].1.stable_id(),
        connection.stable_id()
    );
    change_trusted_device(
        Some(&hb),
        &path,
        TrustedDeviceChange::Revoke { peer: a.node_id() },
    )
    .await
    .unwrap();
    wait_grants(&mut ea, b.node_id(), expected_a).await;
    wait_grants(&mut eb, a.node_id(), expected_b).await;
    assert!(connection.close_reason().is_none());
    hb.change_trusted_device(TrustedDeviceChange::Trust {
        peer: a.node_id(),
        generation,
        display_name: "retained".into(),
    })
    .await
    .unwrap();
    let verifier = RemoteVerifier::create(&SecretPassword::new("Change9".into()).unwrap()).unwrap();
    DesktopConfig::save_remote_auth(&path, verifier.clone()).unwrap();
    hb.update_remote_auth(verifier).await.unwrap();
    assert_eq!(
        crate::node::core::config::SettingsDraft::from_config(
            DesktopConfig::load(&path).unwrap().unwrap()
        )
        .trusted_devices[0]
            .node_id,
        a.node_id().to_hex()
    );
    assert!(
        hb.change_trusted_device(TrustedDeviceChange::Trust {
            peer: a.node_id(),
            generation,
            display_name: "closed".into()
        })
        .await
        .is_err()
    );
    ha.shutdown();
    hb.shutdown();
    drop((ha, hb));
    server.abort();
    std::fs::remove_dir_all(root).unwrap();
}
#[tokio::test]
async fn dead_session_allows_durable_rename_revoke_but_never_trust_or_restart_grant() {
    use crate::node::core::config::{DesktopConfig, SettingsDraft};
    use crate::node::core::remote_auth::AuthorizationGrant;
    use crate::node::core::trusted_devices::TrustedDevice;
    let root = std::env::temp_dir().join(format!("p2p-dead-trust-{:032x}", rand::random::<u128>()));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("settings.json");
    let a = Identity::generate();
    let b = Identity::generate();
    let other = Identity::generate().node_id();
    security_config(
        &path,
        vec![
            TrustedDevice::new(a.node_id(), "same name".into(), Some("100000124".into())),
            TrustedDevice::new(other, "same name".into(), Some("100000124".into())),
        ],
    );
    let load = || SettingsDraft::from_config(DesktopConfig::load(&path).unwrap().unwrap());
    let (signal, server) = start_local_server().await;
    let mut cb = auth_config(signal);
    cb.config_path = Some(path.clone());
    cb.trusted_devices = load().trusted_devices;
    let (hb, mut eb) = spawn(b.clone(), cb).unwrap();
    let (ha, mut ea) = spawn(a.clone(), auth_config(signal)).unwrap();
    wait_signal_online(&mut eb).await;
    wait_signal_online(&mut ea).await;
    ha.connect_peer_trusted(b.node_id()).unwrap();
    let only_trust = AuthorizationGrant {
        password: false,
        trusted_device: true,
    };
    wait_grants(
        &mut ea,
        b.node_id(),
        RemoteAuthorization {
            inbound: AuthorizationGrant::default(),
            outbound: only_trust,
        },
    )
    .await;
    wait_grants(
        &mut eb,
        a.node_id(),
        RemoteAuthorization {
            inbound: only_trust,
            outbound: AuthorizationGrant::default(),
        },
    )
    .await;
    let generation = inspect(&hb).await[&a.node_id()].0;
    let error = hb
        .change_trusted_device(TrustedDeviceChange::Trust {
            peer: a.node_id(),
            generation,
            display_name: "same name".into(),
        })
        .await
        .unwrap_err();
    assert!(
        error.contains("本机密码"),
        "Trusted-only must fail the password gate: {error}"
    );
    hb.shutdown();
    ha.shutdown();
    time::timeout(Duration::from_secs(5), async {
        while hb.is_running() || ha.is_running() {
            time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("sessions must close their command channels");
    // Keep the closed handle, exactly as the shell does after network exit.
    let renamed = change_trusted_device(
        Some(&hb),
        &path,
        TrustedDeviceChange::Rename {
            peer: a.node_id(),
            display_name: "renamed offline".into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(renamed[0].display_name, "renamed offline");
    assert_eq!(load().trusted_devices, renamed);
    let saved = std::fs::read(&path).unwrap();
    assert!(
        change_trusted_device(
            Some(&hb),
            &path,
            TrustedDeviceChange::Trust {
                peer: Identity::generate().node_id(),
                generation,
                display_name: "new offline".into(),
            }
        )
        .await
        .is_err()
    );
    assert!(
        change_trusted_device(
            None,
            &path,
            TrustedDeviceChange::Trust {
                peer: a.node_id(),
                generation,
                display_name: "no session".into(),
            }
        )
        .await
        .is_err()
    );
    assert!(
        change_trusted_device(
            Some(&hb),
            &path,
            TrustedDeviceChange::Rename {
                peer: a.node_id(),
                display_name: "\n".into(),
            }
        )
        .await
        .is_err()
    );
    assert_eq!(std::fs::read(&path).unwrap(), saved);
    #[cfg(unix)]
    {
        let link = root.join("linked.json");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(
            change_trusted_device(
                Some(&hb),
                &link,
                TrustedDeviceChange::Revoke { peer: a.node_id() }
            )
            .await
            .is_err()
        );
        assert_eq!(std::fs::read(&path).unwrap(), saved);
    }
    std::fs::write(&path, b"invalid config").unwrap();
    assert!(
        change_trusted_device(
            Some(&hb),
            &path,
            TrustedDeviceChange::Revoke { peer: a.node_id() }
        )
        .await
        .is_err()
    );
    assert_eq!(std::fs::read(&path).unwrap(), b"invalid config");
    std::fs::write(&path, &saved).unwrap();
    let revoked = change_trusted_device(
        Some(&hb),
        &path,
        TrustedDeviceChange::Revoke { peer: a.node_id() },
    )
    .await
    .unwrap();
    assert_eq!(revoked.len(), 1);
    assert_eq!(revoked[0].node_id, other.to_hex());
    assert_eq!(load().trusted_devices, revoked);
    let renamed = change_trusted_device(
        None,
        &path,
        TrustedDeviceChange::Rename {
            peer: other,
            display_name: "no session rename".into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(load().trusted_devices, renamed);
    drop((ha, hb));
    // New real QUIC/Ed25519 sessions load the edited file: no password and
    // no trusted identity for A must fail, despite matching display metadata.
    let mut cb = auth_config(signal);
    cb.config_path = Some(path.clone());
    cb.trusted_devices = load().trusted_devices;
    let (hb, mut eb) = spawn(b.clone(), cb).unwrap();
    let (ha, mut ea) = spawn(a.clone(), auth_config(signal)).unwrap();
    wait_signal_online(&mut eb).await;
    wait_signal_online(&mut ea).await;
    ha.connect_peer_trusted(b.node_id()).unwrap();
    time::timeout(Duration::from_secs(20), async {
        while let Some(event) = ea.recv().await {
            match event {
                SessionEvent::PeerState {
                    peer,
                    state: PeerLifecycle::Failed(detail),
                    ..
                } if peer == b.node_id() => {
                    assert!(
                        detail.contains("认证失败"),
                        "expected authentication denial, got: {detail}"
                    );
                    return;
                }
                SessionEvent::PeerState {
                    peer,
                    state: PeerLifecycle::Connected(auth),
                    ..
                } if peer == b.node_id() => {
                    panic!("revoked peer reauthorized after restart: {auth:?}")
                }
                _ => {}
            }
        }
        panic!("restarted session closed before authorization failure");
    })
    .await
    .expect("passwordless revoked peer must fail on a fresh transport");
    ha.shutdown();
    hb.shutdown();
    drop((ha, hb));
    server.abort();
    std::fs::remove_dir_all(root).unwrap();
}
#[tokio::test]
async fn restart_auto_start_uses_fresh_identity_bound_trust_and_revoke_closes_running_tunnel() {
    use crate::node::core::config::{AllowedForwardTarget, DesktopConfig, TunnelRule};
    use crate::node::core::remote_auth::AuthorizationGrant;
    use crate::node::core::trusted_devices::TrustedDevice;
    let root = std::env::temp_dir().join(format!(
        "p2p-trusted-restart-{:032x}",
        rand::random::<u128>()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join("settings.json");
    let a = Identity::generate();
    let b = Identity::generate();
    security_config(
        &path,
        vec![TrustedDevice::new(
            a.node_id(),
            "device".into(),
            Some("100000124".into()),
        )],
    );
    let (signal, server) = start_local_server().await;
    let target = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let probe = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let listen = probe.local_addr().unwrap();
    drop(probe);
    let mut rule = TunnelRule::new(
        "trusted echo",
        b.node_id().to_hex(),
        listen.port(),
        target.local_addr().unwrap(),
    );
    rule.auto_start = true;
    let mut ca = auth_config(signal);
    ca.tunnel_rules = vec![rule.clone()];
    let mut cb = auth_config(signal);
    cb.config_path = Some(path.clone());
    cb.trusted_devices = DesktopConfig::load(&path)
        .unwrap()
        .map(crate::node::core::config::SettingsDraft::from_config)
        .unwrap()
        .trusted_devices;
    cb.allowed_forward_targets = vec![AllowedForwardTarget::new(
        "echo",
        target.local_addr().unwrap(),
        vec![a.node_id().to_hex()],
    )];
    // Spawn the persisted owner first, then boot the passwordless auto-start client.
    let (hb, mut eb) = spawn(b.clone(), cb).unwrap();
    wait_signal_online(&mut eb).await;
    let (ha, mut ea) = spawn(a.clone(), ca).unwrap();
    wait_specific_tunnel_state(&mut ea, &rule.id, TunnelRuntimeState::Running).await;
    let only_trust = AuthorizationGrant {
        password: false,
        trusted_device: true,
    };
    wait_grants(
        &mut eb,
        a.node_id(),
        RemoteAuthorization {
            inbound: only_trust,
            outbound: AuthorizationGrant::default(),
        },
    )
    .await;
    let connection = inspect(&hb).await[&a.node_id()].1.clone();
    let mut tcp = tokio::net::TcpStream::connect(listen).await.unwrap();
    tcp.write_all(b"request").await.unwrap();
    let (mut destination, _) = time::timeout(Duration::from_secs(3), target.accept())
        .await
        .unwrap()
        .unwrap();
    let mut bytes = [0; 7];
    destination.read_exact(&mut bytes).await.unwrap();
    assert_eq!(&bytes, b"request");
    hb.change_trusted_device(TrustedDeviceChange::Revoke { peer: a.node_id() })
        .await
        .unwrap();
    wait_specific_tunnel_state(&mut ea, &rule.id, TunnelRuntimeState::WaitingAuthorization).await;
    time::timeout(Duration::from_secs(3), async {
        loop {
            if tokio::net::TcpStream::connect(listen).await.is_err() {
                break;
            }
            time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let mut tail = Vec::new();
    let result = time::timeout(Duration::from_secs(3), tcp.read_to_end(&mut tail)).await;
    assert!(result.is_ok());
    assert!(tail.is_empty());
    assert!(connection.close_reason().is_none()); // Unrelated reverse rights are not killed by closing the whole transport.
    assert!(
        crate::node::core::config::SettingsDraft::from_config(
            DesktopConfig::load(&path).unwrap().unwrap()
        )
        .trusted_devices
        .is_empty()
    );
    // Explicit password reconnect restores the listener; removed trust never follows it.
    ha.connect_peer_with_password(b.node_id(), test_password())
        .unwrap();
    wait_specific_tunnel_state(&mut ea, &rule.id, TunnelRuntimeState::Running).await;
    wait_grants(
        &mut eb,
        a.node_id(),
        RemoteAuthorization {
            inbound: AuthorizationGrant::password(true),
            outbound: AuthorizationGrant::default(),
        },
    )
    .await;
    ha.shutdown();
    hb.shutdown();
    drop((ha, hb));
    server.abort();
    std::fs::remove_dir_all(root).unwrap();
}
#[tokio::test]
async fn revoking_trusted_file_direction_keeps_reverse_password_transfer_alive() {
    use crate::node::core::remote_auth::AuthorizationGrant;
    use crate::node::core::task_model::TaskState;
    use crate::node::core::task_store::TaskStore;
    use crate::node::core::transfer::TransferService;
    use crate::node::core::trusted_devices::TrustedDevice;
    let root =
        std::env::temp_dir().join(format!("p2p-trusted-files-{:032x}", rand::random::<u128>()));
    std::fs::create_dir_all(root.join("a-receive")).unwrap();
    std::fs::create_dir_all(root.join("b-receive")).unwrap();
    let (a_store, _) = TaskStore::open(&root.join("a-state/tasks.json")).unwrap();
    let (b_store, _) = TaskStore::open(&root.join("b-state/tasks.json")).unwrap();
    let a_service = TransferService::new(a_store, root.join("a-receive"));
    let b_service = TransferService::new(b_store, root.join("b-receive"));
    let a = Identity::generate();
    let b = Identity::generate();
    let device = TrustedDevice::new(a.node_id(), "device".into(), None);
    let path = root.join("settings.json");
    security_config(&path, vec![device.clone()]);
    let (signal, server) = start_local_server().await;
    let mut ca = auth_config(signal);
    ca.transfer = Some(a_service.clone());
    let mut cb = auth_config(signal);
    cb.transfer = Some(b_service.clone());
    cb.trusted_devices = vec![device];
    cb.config_path = Some(path);
    let (ha, mut ea) = spawn(a.clone(), ca).unwrap();
    let (hb, mut eb) = spawn(b.clone(), cb).unwrap();
    wait_signal_online(&mut ea).await;
    wait_signal_online(&mut eb).await;
    hb.connect_peer_with_password(a.node_id(), test_password())
        .unwrap();
    let trusted = AuthorizationGrant {
        password: false,
        trusted_device: true,
    };
    wait_grants(
        &mut ea,
        b.node_id(),
        RemoteAuthorization {
            inbound: AuthorizationGrant::password(true),
            outbound: trusted,
        },
    )
    .await;
    wait_grants(
        &mut eb,
        a.node_id(),
        RemoteAuthorization {
            inbound: trusted,
            outbound: AuthorizationGrant::password(true),
        },
    )
    .await;
    let (a_reached, a_wait) = oneshot::channel();
    let (a_release, a_resume) = oneshot::channel();
    *a_service.first_chunk_gate.lock().unwrap() = Some((a_reached, a_resume));
    let (b_reached, b_wait) = oneshot::channel();
    let (b_release, b_resume) = oneshot::channel();
    *b_service.first_chunk_gate.lock().unwrap() = Some((b_reached, b_resume));
    let bytes = vec![0x5a; 1024 * 1024];
    std::fs::write(root.join("trusted.bin"), &bytes).unwrap();
    std::fs::write(root.join("password.bin"), &bytes).unwrap();
    ha.send_file(b.node_id(), root.join("trusted.bin")).unwrap();
    hb.send_file(a.node_id(), root.join("password.bin"))
        .unwrap();
    time::timeout(Duration::from_secs(10), async {
        a_wait.await.unwrap();
        b_wait.await.unwrap();
    })
    .await
    .unwrap();
    hb.change_trusted_device(TrustedDeviceChange::Revoke { peer: a.node_id() })
        .await
        .unwrap();
    wait_grants(
        &mut ea,
        b.node_id(),
        RemoteAuthorization {
            inbound: AuthorizationGrant::password(true),
            outbound: AuthorizationGrant::default(),
        },
    )
    .await;
    a_release.send(()).unwrap();
    time::timeout(Duration::from_secs(10), async {
        loop {
            let tasks = a_service.snapshot().await.unwrap();
            let remote = b_service.snapshot().await.unwrap();
            let kept = tasks.iter().any(|t| {
                t.direction() == crate::node::core::task_model::TaskDirection::Receive
                    && t.state() == TaskState::Completed
            });
            let revoked = remote.iter().any(|t| {
                t.direction() == crate::node::core::task_model::TaskDirection::Receive
                    && t.state() == TaskState::Interrupted
            });
            if kept && revoked {
                break;
            }
            time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(b_release.send(()).is_err());
    assert_eq!(
        std::fs::read(root.join("a-receive/password.bin")).unwrap(),
        bytes
    );
    assert!(!root.join("b-receive/trusted.bin").exists());
    assert!(inspect(&ha).await[&b.node_id()].1.close_reason().is_none());
    ha.shutdown();
    hb.shutdown();
    drop((ha, hb));
    server.abort();
    drop((a_service, b_service));
    std::fs::remove_dir_all(root).unwrap();
}
#[tokio::test]
async fn explicit_passwordless_reconnect_does_not_reuse_old_in_memory_password() {
    let (signal, server) = start_local_server().await;
    let a = Identity::generate();
    let b = Identity::generate();
    let (ha, mut ea) = spawn(a.clone(), auth_config(signal)).unwrap();
    let (hb, mut eb) = spawn(b.clone(), auth_config(signal)).unwrap();
    wait_signal_online(&mut ea).await;
    wait_signal_online(&mut eb).await;
    ha.connect_peer_with_password(b.node_id(), test_password())
        .unwrap();
    wait_authorization(&mut ea, b.node_id()).await;
    wait_authorization(&mut eb, a.node_id()).await;
    inspect(&ha).await[&b.node_id()]
        .1
        .close(0u32.into(), b"test fresh passwordless session");
    ha.connect_peer_trusted(b.node_id()).unwrap();
    wait_failed(&mut ea).await;
    wait_failed(&mut eb).await;
    assert!(inspect(&ha).await.is_empty());
    assert!(inspect(&hb).await.is_empty());
    ha.shutdown();
    hb.shutdown();
    drop((ha, hb));
    server.abort();
}
