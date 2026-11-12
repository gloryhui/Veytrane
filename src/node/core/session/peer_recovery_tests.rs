use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

#[tokio::test]
async fn simultaneous_private_socket_recovery_uses_primary_probe_bridges() {
    for family in [
        crate::net::NetworkFamilies::Ipv4Only,
        crate::net::NetworkFamilies::Ipv6Only,
    ] {
        if family == crate::net::NetworkFamilies::Ipv6Only
            && !crate::net::family::ipv6_test_available()
        {
            continue;
        }
        let config = DesktopNetworkConfig {
            families: family,
            stun_servers: Vec::new(),
            include_loopback: true,
            ..Default::default()
        };
        let a = Identity::generate();
        let b = Identity::generate();
        let ap = prepare_desktop_network(&config).await.unwrap();
        let af = prepare_desktop_network(&config).await.unwrap();
        let bp = prepare_desktop_network(&config).await.unwrap();
        let bf = prepare_desktop_network(&config).await.unwrap();
        let token = PunchToken::random();
        let (a_in, a_rx) = mpsc::channel(MAX_CANDIDATES);
        let (b_in, b_rx) = mpsc::channel(MAX_CANDIDATES);
        let mut accepts = JoinSet::new();
        for (network, route, sender, peer) in [
            (&ap, 1, &a_in, b.node_id()),
            (&af, 2, &a_in, b.node_id()),
            (&bp, 1, &b_in, a.node_id()),
            (&bf, 2, &b_in, a.node_id()),
        ] {
            let path = network.paths[0].clone();
            let sender = sender.clone();
            accepts.spawn(async move {
                while let Some(incoming) = path.endpoint.accept().await {
                    if path
                        .punch_socket
                        .claim_authorized_peer(incoming.remote_address())
                        == Some((peer, 1))
                    {
                        if let Err(error) = sender.send((route, incoming)).await {
                            error.0.1.refuse();
                            break;
                        }
                    } else {
                        incoming.refuse();
                    }
                }
            });
        }
        let address = |network: &DesktopNetwork| {
            SocketAddr::new(
                if family == crate::net::NetworkFamilies::Ipv6Only {
                    "::1"
                } else {
                    "127.0.0.1"
                }
                .parse()
                .unwrap(),
                network.paths[0].local_addr.port(),
            )
        };
        let result = time::timeout(Duration::from_secs(15), async {
            tokio::join!(
                race_peer_paths(
                    2,
                    vec![(af.paths[0].clone(), vec![address(&bp)])],
                    ap.paths.clone(),
                    token,
                    b.node_id(),
                    1,
                    a.node_id(),
                    a.clone(),
                    a_rx,
                    None,
                ),
                race_peer_paths(
                    2,
                    vec![(bf.paths[0].clone(), vec![address(&ap)])],
                    bp.paths.clone(),
                    token,
                    a.node_id(),
                    1,
                    b.node_id(),
                    b.clone(),
                    b_rx,
                    None,
                ),
            )
        })
        .await
        .expect(
            "both sides must discover fresh probe sources without changing published candidates",
        );
        let (ar, ac) = result.0.unwrap();
        let (br, bc) = result.1.unwrap();
        if should_initiate_quic(a.node_id(), b.node_id()) {
            assert_eq!((ar, br), (2, 1));
            assert_eq!(ac.connection().remote_address(), address(&bp));
            assert_eq!(bc.connection().remote_address(), address(&af));
        } else {
            assert_eq!((ar, br), (1, 2));
            assert_eq!(ac.connection().remote_address(), address(&bf));
            assert_eq!(bc.connection().remote_address(), address(&ap));
        }
        // A completed handshake can already be queued when its attempt is
        // cancelled. It must not publish during the gap before a new generation.
        let mut registry = PeerRegistry::default();
        let BeginPeerAttempt::Started(generation) = registry.begin_attempt(b.node_id()) else {
            panic!("test peer must fit in an empty registry");
        };
        assert!(registry.transition(b.node_id(), generation, PeerLifecycle::Disconnected));
        assert!(!transport_can_publish(
            &registry,
            b.node_id(),
            generation,
            ac.connection()
        ));
        assert!(ac.connection().close_reason().is_some());
        drop((ac, bc));
        accepts.abort_all();
        while accepts.join_next().await.is_some() {}
        for network in [&ap, &af, &bp, &bf] {
            network.close();
        }
    }
}

#[tokio::test]
async fn expired_pending_lookup_releases_its_private_socket() {
    let config = DesktopNetworkConfig {
        families: crate::net::NetworkFamilies::Ipv4Only,
        stun_servers: Vec::new(),
        include_loopback: true,
        ..Default::default()
    };
    let network = prepare_desktop_network(&config).await.unwrap();
    let address = network.paths[0].local_addr;
    let peer = Identity::generate().node_id();
    let mut networks = HashMap::from([(
        peer,
        RecoveryNetwork {
            route: 2,
            network,
            accept_tasks: JoinSet::new(),
            keepalives: Vec::new(),
        },
    )]);
    let mut pending = HashSet::from([peer]);
    release_unused_recovery_networks(&mut networks, &HashMap::new(), &HashMap::new(), &pending)
        .await;
    assert_eq!(
        networks.len(),
        1,
        "an outstanding lookup still owns its socket"
    );
    pending.remove(&peer);
    release_unused_recovery_networks(&mut networks, &HashMap::new(), &HashMap::new(), &pending)
        .await;
    assert!(networks.is_empty());
    // Quinn's socket driver relinquishes its final handle asynchronously.
    time::timeout(Duration::from_secs(3), async {
        loop {
            match std::net::UdpSocket::bind(address) {
                Ok(socket) => {
                    drop(socket);
                    break;
                }
                Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => {
                    time::sleep(Duration::from_millis(10)).await;
                }
                Err(error) => panic!("unexpected UDP release error: {error}"),
            }
        }
    })
    .await
    .expect("the private UDP port must be released");
}

#[test]
fn manual_reconnect_cancels_preparation_but_automatic_retry_does_not() {
    let peer = Identity::generate().node_id();
    let other = Identity::generate().node_id();
    let (done, _) = oneshot::channel();
    assert!(recovery_preparation_invalidated(
        &SessionCommand::DisconnectPeer { peer, done },
        peer
    ));
    let (done, _) = oneshot::channel();
    assert!(!recovery_preparation_invalidated(
        &SessionCommand::DisconnectPeer { peer: other, done },
        peer
    ));
    assert!(recovery_preparation_invalidated(
        &SessionCommand::ConnectAuthenticated {
            peer,
            password: test_password()
        },
        peer
    ));
    assert!(!recovery_preparation_invalidated(
        &SessionCommand::ConnectPeer(peer),
        peer
    ));
}

// Exercise the actual maintenance loop and network handshakes. The offline peer
// remains registered with an unreachable UDP candidate, reproducing a stale NAT
// mapping rather than the different "peer not registered" case.
#[tokio::test]
async fn failed_peer_automatically_recovers_without_breaking_another_peers_tcp_stream() {
    for local_initiates in [true, false] {
        let (signal, server) = start_local_server().await;
        let a = Identity::generate();
        let b = Identity::generate();
        let mut c = Identity::generate();
        while should_initiate_quic(a.node_id(), c.node_id()) != local_initiates {
            c = Identity::generate();
        }
        let echo = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let target = echo.local_addr().unwrap();
        let echo_task = tokio::spawn(async move {
            while let Ok((mut stream, _)) = echo.accept().await {
                tokio::spawn(async move {
                    let (mut r, mut w) = stream.split();
                    let _ = tokio::io::copy(&mut r, &mut w).await;
                });
            }
        });
        let reserve = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let listen = reserve.local_addr().unwrap();
        drop(reserve);
        let mut rule = crate::node::core::config::TunnelRule::new(
            "healthy-during-peer-recovery",
            b.node_id().to_hex(),
            listen.port(),
            target,
        );
        rule.auto_start = true;
        let mut ca = local_config(signal);
        ca.network.families = crate::net::NetworkFamilies::Ipv4Only;
        ca.tunnel_rules.push(rule.clone());
        let mut cb = local_config(signal);
        cb.network.families = crate::net::NetworkFamilies::Ipv4Only;
        cb.allowed_forward_targets
            .push(service_grant("echo", target, vec![a.node_id().to_hex()]));
        let (hb, mut eb) = spawn(b.clone(), cb).unwrap();
        wait_signal_online(&mut eb).await;
        let (ha, mut ea) = spawn(a.clone(), ca).unwrap();
        wait_specific_tunnel_state(&mut ea, &rule.id, TunnelRuntimeState::Running).await;
        let (_, healthy_a) = inspect(&ha).await[&b.node_id()].clone();
        let (_, healthy_b) = inspect(&hb).await[&a.node_id()].clone();
        let primary_address = healthy_b.remote_address();
        let mut stream = tokio::net::TcpStream::connect(rule.listen).await.unwrap();
        let ticks = Arc::new(AtomicUsize::new(0));
        let tick_count = ticks.clone();
        let (stop, mut stopping) = watch::channel(false);
        let heartbeat = tokio::spawn(async move {
            loop {
                if *stopping.borrow() {
                    return;
                }
                let value = tick_count.load(Ordering::SeqCst).to_le_bytes();
                time::timeout(Duration::from_secs(3), async {
                    stream.write_all(&value).await.unwrap();
                    let mut response = [0; std::mem::size_of::<usize>()];
                    stream.read_exact(&mut response).await.unwrap();
                    assert_eq!(response, value);
                })
                .await
                .expect("the existing TCP stream must remain usable throughout recovery");
                tick_count.fetch_add(1, Ordering::SeqCst);
                tokio::select! {
                    _ = time::sleep(Duration::from_millis(25)) => {},
                    _ = stopping.changed() => {},
                }
            }
        });

        let blackhole = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let stale = SignalingClient::connect_with_events(
            &signal.to_string(),
            &c,
            vec![Candidate::host(blackhole.local_addr().unwrap())],
        )
        .await
        .unwrap();
        ha.connect_peer(c.node_id()).unwrap();
        time::timeout(Duration::from_secs(150), async {
            while let Some(event) = ea.recv().await {
                if let SessionEvent::Diagnostic(detail) = event
                    && detail.contains("已准备独立恢复路径")
                {
                    return;
                }
            }
            panic!("session stopped before automatic per-peer recovery");
        })
        .await
        .expect("transport failures must trigger automatic recovery despite a healthy third peer");

        // Even an explicit address check from the healthy peer must observe the
        // unchanged published primary candidates, including with an older peer.
        hb.connect_peer(a.node_id()).unwrap();
        let mut cc = local_config(signal);
        cc.network.families = crate::net::NetworkFamilies::Ipv4Only;
        let (hc, mut ec) = spawn(c.clone(), cc).unwrap();
        wait_signal_online(&mut ec).await;
        // Failures from the unreachable candidate can still be queued or in
        // flight. They are expected until maintenance obtains the fresh offer.
        time::timeout(Duration::from_secs(60), async {
            while let Some(event) = ea.recv().await {
                if matches!(event, SessionEvent::PeerState {
                    peer, state: PeerLifecycle::Connected(_), ..
                } if peer == c.node_id())
                {
                    return;
                }
            }
            panic!("session ended before the failed peer recovered");
        })
        .await
        .expect("the failed peer must reconnect automatically after becoming reachable");
        wait_connected(&mut ec, &[a.node_id()]).await;
        drop(stale);
        assert_eq!(
            inspect(&ha).await[&b.node_id()].1.stable_id(),
            healthy_a.stable_id()
        );
        assert_eq!(
            inspect(&hb).await[&a.node_id()].1.stable_id(),
            healthy_b.stable_id()
        );
        assert!(healthy_a.close_reason().is_none());
        assert!(healthy_b.close_reason().is_none());
        if local_initiates {
            assert_ne!(
                inspect(&hc).await[&a.node_id()].1.remote_address(),
                primary_address,
                "the recovered initiator must use its fresh UDP socket"
            );
        }
        assert!(ticks.load(Ordering::SeqCst) > 20);
        if local_initiates {
            let private_address = inspect(&hc).await[&a.node_id()].1.remote_address();
            ha.disconnect_peer(c.node_id()).await.unwrap();
            time::timeout(Duration::from_secs(8), async {
                loop {
                    match std::net::UdpSocket::bind(private_address) {
                        Ok(socket) => {
                            drop(socket);
                            break;
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => {
                            time::sleep(Duration::from_millis(20)).await;
                        }
                        Err(error) => panic!("unexpected private socket cleanup error: {error}"),
                    }
                }
            })
            .await
            .expect("disconnect must release the private recovery socket");
            assert!(healthy_a.close_reason().is_none());
        }
        stop.send_replace(true);
        heartbeat.await.unwrap();
        ha.shutdown();
        hb.shutdown();
        hc.shutdown();
        drop((ha, hb, hc));
        echo_task.abort();
        server.abort();
    }
}
