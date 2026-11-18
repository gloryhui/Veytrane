use super::*;
use std::sync::{
    Mutex,
    atomic::{AtomicUsize, Ordering},
};

#[derive(Clone, Debug, Default)]
struct ProxyStats {
    received: usize,
    forwarded: usize,
    dropped: usize,
    unknown_sources: usize,
    dropped_sources: HashSet<SocketAddr>,
}

fn loopback_address(address: SocketAddr) -> SocketAddr {
    SocketAddr::new(
        if address.is_ipv6() {
            "::1".parse().unwrap()
        } else {
            "127.0.0.1".parse().unwrap()
        },
        address.port(),
    )
}

async fn run_restricted_proxy(
    proxy: Arc<tokio::net::UdpSocket>,
    private_target: SocketAddr,
    proxies_by_private: Arc<HashMap<SocketAddr, Arc<tokio::net::UdpSocket>>>,
    allowed: Arc<Mutex<HashMap<SocketAddr, HashSet<SocketAddr>>>>,
    stats: Arc<Mutex<HashMap<SocketAddr, ProxyStats>>>,
) {
    let own_public = proxy.local_addr().unwrap();
    let mut buffer = vec![0; 65_535];
    loop {
        let (length, source) = match proxy.recv_from(&mut buffer).await {
            Ok(received) => received,
            Err(_) => return,
        };
        let Some(sender_proxy) = proxies_by_private.get(&source).cloned() else {
            let mut current = stats.lock().unwrap();
            let entry = current.entry(own_public).or_default();
            entry.received += 1;
            entry.unknown_sources += 1;
            continue;
        };
        let sender_public = sender_proxy.local_addr().unwrap();

        let forward = {
            let mut current = allowed.lock().unwrap();
            current.entry(source).or_default().insert(own_public);
            current
                .get(&private_target)
                .is_some_and(|sources| sources.contains(&sender_public))
        };

        {
            let mut current = stats.lock().unwrap();
            let entry = current.entry(own_public).or_default();
            entry.received += 1;
            if forward {
                entry.forwarded += 1;
            } else {
                entry.dropped += 1;
                entry.dropped_sources.insert(source);
            }
        }

        if forward {
            let _ = sender_proxy
                .send_to(&buffer[..length], private_target)
                .await;
        }
    }
}

#[allow(clippy::type_complexity)]
async fn restricted_nat_harness() -> (
    Vec<DesktopNetwork>,
    HashMap<SocketAddr, SocketAddr>,
    Arc<Mutex<HashMap<SocketAddr, HashSet<SocketAddr>>>>,
    Arc<Mutex<HashMap<SocketAddr, ProxyStats>>>,
    JoinSet<()>,
) {
    let config = DesktopNetworkConfig {
        families: crate::net::NetworkFamilies::Ipv4Only,
        stun_servers: Vec::new(),
        include_loopback: true,
        ..Default::default()
    };
    let networks = vec![
        prepare_desktop_network(&config).await.unwrap(),
        prepare_desktop_network(&config).await.unwrap(),
        prepare_desktop_network(&config).await.unwrap(),
        prepare_desktop_network(&config).await.unwrap(),
    ];
    let mut proxies_by_private = HashMap::new();
    let mut proxies = Vec::new();
    for network in &networks {
        let private = loopback_address(network.paths[0].local_addr);
        let proxy = Arc::new(tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap());
        proxies_by_private.insert(private, proxy.clone());
        proxies.push((private, proxy));
    }

    let proxies_by_private = Arc::new(proxies_by_private);
    let public_by_private = proxies_by_private
        .iter()
        .map(|(private, proxy)| (*private, proxy.local_addr().unwrap()))
        .collect::<HashMap<_, _>>();
    let allowed = Arc::new(Mutex::new(HashMap::new()));
    let stats = Arc::new(Mutex::new(
        public_by_private
            .values()
            .copied()
            .map(|public| (public, ProxyStats::default()))
            .collect::<HashMap<_, _>>(),
    ));
    let mut tasks = JoinSet::new();
    for (private, proxy) in proxies {
        tasks.spawn(run_restricted_proxy(
            proxy.clone(),
            private,
            proxies_by_private.clone(),
            allowed.clone(),
            stats.clone(),
        ));
    }
    (networks, public_by_private, allowed, stats, tasks)
}

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
        let ar = prepare_desktop_network(&config).await.unwrap();
        let bp = prepare_desktop_network(&config).await.unwrap();
        let br = prepare_desktop_network(&config).await.unwrap();
        let token = PunchToken::random();
        let (a_in, a_rx) = mpsc::channel(MAX_CANDIDATES);
        let (b_in, b_rx) = mpsc::channel(MAX_CANDIDATES);
        let mut accepts = JoinSet::new();
        for (network, route, sender, peer) in [
            (&ap, 1, &a_in, b.node_id()),
            (&ar, 2, &a_in, b.node_id()),
            (&bp, 1, &b_in, a.node_id()),
            (&br, 2, &b_in, a.node_id()),
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
                    vec![
                        AttemptPath {
                            route: 1,
                            path: ap.paths[0].clone(),
                            candidates: vec![address(&bp)],
                        },
                        AttemptPath {
                            route: 2,
                            path: ar.paths[0].clone(),
                            candidates: vec![address(&bp)],
                        },
                    ],
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
                    vec![
                        AttemptPath {
                            route: 1,
                            path: bp.paths[0].clone(),
                            candidates: vec![address(&ap)],
                        },
                        AttemptPath {
                            route: 2,
                            path: br.paths[0].clone(),
                            candidates: vec![address(&ap)],
                        },
                    ],
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
            "both sides must race every local path without changing published primary candidates",
        );
        let (a_route, a_guard) = result.0.unwrap();
        let (b_route, b_guard) = result.1.unwrap();
        let a_remote = match b_route {
            1 => address(&bp),
            2 => address(&br),
            _ => panic!("unexpected B route {b_route}"),
        };
        let b_remote = match a_route {
            1 => address(&ap),
            2 => address(&ar),
            _ => panic!("unexpected A route {a_route}"),
        };
        assert_eq!(a_guard.connection().remote_address(), a_remote);
        assert_eq!(b_guard.connection().remote_address(), b_remote);
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
            a_guard.connection()
        ));
        assert!(a_guard.connection().close_reason().is_some());
        drop((a_guard, b_guard));
        accepts.abort_all();
        while accepts.join_next().await.is_some() {}
        for network in [&ap, &ar, &bp, &br] {
            network.close();
        }
    }
}

#[tokio::test]
async fn port_restricted_recovery_races_primary_for_both_quic_roles() {
    for a_initiates in [false, true] {
        let a = Identity::generate();
        let mut b = Identity::generate();
        while should_initiate_quic(a.node_id(), b.node_id()) != a_initiates {
            b = Identity::generate();
        }
        let token = PunchToken::random();
        let (networks, public, allowed, stats, mut proxies) = restricted_nat_harness().await;
        let (ap, ar, bp, br) = (&networks[0], &networks[1], &networks[2], &networks[3]);
        let a_primary_public = public[&loopback_address(ap.paths[0].local_addr)];
        let b_primary_public = public[&loopback_address(bp.paths[0].local_addr)];
        let a_candidates = vec![Candidate::host(a_primary_public)];
        let b_candidates = vec![Candidate::host(b_primary_public)];
        let a_primary_private = loopback_address(ap.paths[0].local_addr);
        let a_recovery_private = loopback_address(ar.paths[0].local_addr);
        let b_primary_private = loopback_address(bp.paths[0].local_addr);
        let b_recovery_private = loopback_address(br.paths[0].local_addr);
        let (a_in, a_rx) = mpsc::channel(MAX_CANDIDATES);
        let (b_in, b_rx) = mpsc::channel(MAX_CANDIDATES);
        let mut accepts = JoinSet::new();
        for (network, route, sender, peer) in [
            (ap, 1, &a_in, b.node_id()),
            (ar, 2, &a_in, b.node_id()),
            (bp, 1, &b_in, a.node_id()),
            (br, 2, &b_in, a.node_id()),
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

        let a_paths = peer_attempt_paths(2, ar, Some((1, ap)), &b_candidates);
        let b_paths = peer_attempt_paths(2, br, Some((1, bp)), &a_candidates);
        let result = time::timeout(Duration::from_secs(15), async {
            tokio::join!(
                race_peer_paths(
                    2,
                    a_paths,
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
                    b_paths,
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
        .expect("port-restricted primary/recovery race must complete");
        let (ar, ac) = result.0.unwrap();
        let (br, bc) = result.1.unwrap();
        assert_eq!((ar, br), (1, 1));
        assert_eq!(ac.connection().remote_address(), b_primary_public);
        assert_eq!(bc.connection().remote_address(), a_primary_public);
        assert!(ac.connection().close_reason().is_none());
        assert!(bc.connection().close_reason().is_none());
        {
            let current = stats.lock().unwrap();
            for endpoint in [a_primary_public, b_primary_public] {
                let endpoint_stats = &current[&endpoint];
                assert!(
                    endpoint_stats.forwarded > 0,
                    "primary proxy must forward reciprocal traffic"
                );
                assert!(
                    endpoint_stats.dropped > 0,
                    "recovery traffic toward a primary-only allowed set must be dropped"
                );
                assert_eq!(endpoint_stats.unknown_sources, 0);
            }
            assert!(
                current[&b_primary_public]
                    .dropped_sources
                    .contains(&a_recovery_private)
            );
            assert!(
                current[&a_primary_public]
                    .dropped_sources
                    .contains(&b_recovery_private)
            );
        }
        {
            let current = allowed.lock().unwrap();
            let a_primary_allowed = &current[&a_primary_private];
            let b_primary_allowed = &current[&b_primary_private];
            assert_eq!(a_primary_allowed.len(), 1);
            assert!(a_primary_allowed.contains(&b_primary_public));
            assert!(!a_primary_allowed.contains(&public[&b_recovery_private]));
            assert_eq!(b_primary_allowed.len(), 1);
            assert!(b_primary_allowed.contains(&a_primary_public));
            assert!(!b_primary_allowed.contains(&public[&a_recovery_private]));
        }
        drop((ac, bc));
        accepts.abort_all();
        while accepts.join_next().await.is_some() {}
        proxies.abort_all();
        while proxies.join_next().await.is_some() {}
        for network in &networks {
            network.close();
        }
    }
}

#[tokio::test]
async fn port_restricted_recovery_only_cannot_pass_for_either_quic_role() {
    for a_initiates in [false, true] {
        let a = Identity::generate();
        let mut b = Identity::generate();
        while should_initiate_quic(a.node_id(), b.node_id()) != a_initiates {
            b = Identity::generate();
        }
        let token = PunchToken::random();
        let (networks, public, _, stats, mut proxies) = restricted_nat_harness().await;
        let (ar, br) = (&networks[1], &networks[3]);
        let (a_primary_public, b_primary_public) = (
            public[&loopback_address(networks[0].paths[0].local_addr)],
            public[&loopback_address(networks[2].paths[0].local_addr)],
        );
        let a_candidates = vec![Candidate::host(a_primary_public)];
        let b_candidates = vec![Candidate::host(b_primary_public)];
        let (a_in, a_rx) = mpsc::channel(MAX_CANDIDATES);
        let (b_in, b_rx) = mpsc::channel(MAX_CANDIDATES);
        let mut accepts = JoinSet::new();
        for (network, route, sender, peer) in [
            (&networks[0], 1, &a_in, b.node_id()),
            (ar, 2, &a_in, b.node_id()),
            (&networks[2], 1, &b_in, a.node_id()),
            (br, 2, &b_in, a.node_id()),
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

        let result = time::timeout(Duration::from_secs(3), async {
            tokio::join!(
                race_peer_paths(
                    2,
                    peer_attempt_paths(2, ar, None, &b_candidates),
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
                    peer_attempt_paths(2, br, None, &a_candidates),
                    token,
                    a.node_id(),
                    1,
                    b.node_id(),
                    b.clone(),
                    b_rx,
                    None,
                )
            )
        })
        .await;
        assert!(
            result.is_err(),
            "recovery-only paths must not complete against primary-only filtered candidates"
        );
        {
            let current = stats.lock().unwrap();
            for endpoint in [a_primary_public, b_primary_public] {
                let endpoint_stats = &current[&endpoint];
                assert!(endpoint_stats.dropped > 0);
                assert_eq!(
                    endpoint_stats.forwarded, 0,
                    "no recovery-only datagram may cross the restricted proxy"
                );
                assert_eq!(endpoint_stats.unknown_sources, 0);
            }
        }
        accepts.abort_all();
        while accepts.join_next().await.is_some() {}
        proxies.abort_all();
        while proxies.join_next().await.is_some() {}
        for network in &networks {
            network.close();
        }
    }
}

#[tokio::test]
async fn clearing_reconnect_probe_releases_old_token_for_same_source() {
    let config = DesktopNetworkConfig {
        families: crate::net::NetworkFamilies::Ipv4Only,
        stun_servers: Vec::new(),
        include_loopback: true,
        ..Default::default()
    };
    let network = prepare_desktop_network(&config).await.unwrap();
    let path = network.paths[0].clone();
    let peer = Identity::generate().node_id();
    let old_token = PunchToken::random();
    let new_token = PunchToken::random();
    let old_receiver = path
        .punch_socket
        .register_peer_probe(&old_token, peer, 1)
        .unwrap();
    let mut reconnect_probes = HashMap::from([(
        peer,
        VecDeque::from([ReconnectProbe {
            generation: 1,
            network: 2,
            candidates: Vec::new(),
            token: old_token,
            receivers: vec![old_receiver],
            deadline: Box::pin(time::sleep(Duration::from_secs(5))),
        }]),
    )]);
    let mut new_receiver = path
        .punch_socket
        .register_peer_probe(&new_token, peer, 2)
        .unwrap();
    let sender = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let destination = loopback_address(path.local_addr);

    sender
        .send_to(&crate::nat::punch::probe_packet(&old_token), destination)
        .await
        .unwrap();
    let source = time::timeout(
        Duration::from_secs(2),
        reconnect_probes.get_mut(&peer).unwrap()[0].receivers[0].recv(),
    )
    .await
    .unwrap()
    .unwrap();
    sender
        .send_to(&crate::nat::punch::probe_packet(&new_token), destination)
        .await
        .unwrap();
    let new_source = time::timeout(Duration::from_secs(2), new_receiver.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(source, new_source);
    assert_eq!(
        path.punch_socket.claim_authorized_peer(source),
        None,
        "the same source must remain ambiguous while both token generations are live"
    );

    reconnect_probes.remove(&peer);
    assert_eq!(
        path.punch_socket.claim_authorized_peer(source),
        Some((peer, 2)),
        "clearing the superseded reconnect watch must release its source claim"
    );
    drop(new_receiver);
    network.close();
}

#[tokio::test]
async fn newer_offer_sends_punch_before_the_previous_round_times_out() {
    let (signal, server) = start_local_server().await;
    for local_initiates in [true, false] {
        let local = Identity::generate();
        let mut peer = Identity::generate();
        while should_initiate_quic(local.node_id(), peer.node_id()) != local_initiates {
            peer = Identity::generate();
        }
        let mut config = local_config(signal);
        config.network.families = crate::net::NetworkFamilies::Ipv4Only;
        let (handle, mut events) = spawn(local.clone(), config).unwrap();
        wait_signal_online(&mut events).await;

        let socket = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let mut client = SignalingClient::connect_with_events(
            &signal.to_string(),
            &peer,
            vec![Candidate::host(socket.local_addr().unwrap())],
        )
        .await
        .unwrap();
        let mut previous = None;
        for _ in 0..2 {
            let crate::discovery::signal::LookupOutcome::Ready(offer) =
                client.lookup(local.node_id()).await.unwrap()
            else {
                panic!("local session already registered");
            };
            assert_ne!(previous, Some(offer.token));
            previous = Some(offer.token);
            time::timeout(Duration::from_secs(2), async {
                let mut buffer = [0; 1024];
                loop {
                    let (len, _) = socket.recv_from(&mut buffer).await.unwrap();
                    if crate::nat::punch::is_probe_with_token(&buffer[..len], &offer.token) {
                        break;
                    }
                }
            })
            .await
            .expect("the local session must send this token without waiting for the old deadline");
        }
        handle.shutdown();
    }
    server.abort();
}

#[tokio::test]
async fn newer_offer_supersedes_probe_only_attempt_for_both_quic_roles() {
    for local_initiates in [false, true] {
        let config = DesktopNetworkConfig {
            families: crate::net::NetworkFamilies::Ipv4Only,
            stun_servers: Vec::new(),
            include_loopback: true,
            ..Default::default()
        };
        let local = Identity::generate();
        let mut remote = Identity::generate();
        while should_initiate_quic(local.node_id(), remote.node_id()) != local_initiates {
            remote = Identity::generate();
        }
        let network = prepare_desktop_network(&config).await.unwrap();
        let path = network.paths[0].clone();
        let sender = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let source = sender.local_addr().unwrap();
        let destination = loopback_address(path.local_addr);
        let old_token = PunchToken::from_bytes([0x31; 16]);
        let child_token = PunchToken::from_bytes([0x32; 16]);
        let new_token = PunchToken::from_bytes([0x33; 16]);
        let old_generation = 1;
        let (cancel, cancelled) = watch::channel(false);
        let (child_ready_tx, child_ready_rx) = oneshot::channel();
        let (registered_tx, registered_rx) = oneshot::channel();
        let (authorized_tx, authorized_rx) = oneshot::channel();
        let task_path = path.clone();
        let task_peer = remote.node_id();
        let join = tokio::spawn(async move {
            let mut cancelled = cancelled;
            let mut children = JoinSet::new();
            let child_path = task_path.clone();
            children.spawn(async move {
                let _receiver = child_path
                    .punch_socket
                    .register_peer_probe(&child_token, task_peer, old_generation)
                    .unwrap();
                let _ = child_ready_tx.send(());
                std::future::pending::<()>().await;
            });
            child_ready_rx.await.unwrap();
            let mut receiver = task_path
                .punch_socket
                .register_peer_probe(&old_token, task_peer, old_generation)
                .unwrap();
            let _ = registered_tx.send(());
            receiver.recv().await.unwrap();
            let _ = authorized_tx.send(());
            let _ = cancelled.changed().await;
            children.abort_all();
            while children.join_next().await.is_some() {}
        });
        registered_rx.await.unwrap();
        sender
            .send_to(&crate::nat::punch::probe_packet(&old_token), destination)
            .await
            .unwrap();
        time::timeout(Duration::from_secs(2), authorized_rx)
            .await
            .expect("old PUNCH token must be routed before supersession")
            .unwrap();
        assert_eq!(
            path.punch_socket.claim_authorized_peer(source),
            Some((remote.node_id(), old_generation))
        );

        let abort = join.abort_handle();
        let mut attempts = HashMap::from([(
            remote.node_id(),
            ActivePeerAttempt {
                generation: old_generation,
                token: old_token,
                progress: AttemptProgress::default(),
                cancel: Some(cancel),
                abort,
                join: Some(join),
            },
        )]);
        let mut peers = PeerRegistry::default();
        let BeginPeerAttempt::Started(generation) = peers.begin_attempt(remote.node_id()) else {
            panic!("first test attempt must start");
        };
        assert_eq!(generation, old_generation);
        assert!(peers.transition(remote.node_id(), generation, PeerLifecycle::Authenticating,));
        assert!(
            !newer_offer_can_supersede(
                Some(&PeerLifecycle::Authenticating),
                old_token,
                old_token,
                &AttemptProgress::default(),
            ),
            "a duplicate token must not churn the active attempt"
        );
        assert!(
            attempts
                .get(&remote.node_id())
                .unwrap()
                .can_supersede(peers.state(remote.node_id()), new_token),
            "a newer token must replace an attempt that has only been probing for both QUIC roles"
        );
        attempts
            .remove(&remote.node_id())
            .unwrap()
            .cancel_and_wait()
            .await;
        assert!(
            attempts.is_empty(),
            "the sole peer_attempts entry owns task and progress"
        );
        assert_eq!(
            path.punch_socket.claim_authorized_peer(source),
            None,
            "awaiting the aborted outer task must synchronously retire its PUNCH authorization"
        );

        let mut new_receiver = path
            .punch_socket
            .register_peer_probe(&new_token, remote.node_id(), 2)
            .unwrap();
        sender
            .send_to(&crate::nat::punch::probe_packet(&child_token), destination)
            .await
            .unwrap();
        assert!(
            time::timeout(Duration::from_millis(100), new_receiver.recv())
                .await
                .is_err(),
            "the aborted task must drop child PUNCH receivers before supersession returns"
        );
        assert_eq!(path.punch_socket.claim_authorized_peer(source), None);
        sender
            .send_to(&crate::nat::punch::probe_packet(&new_token), destination)
            .await
            .unwrap();
        let source = time::timeout(Duration::from_secs(2), new_receiver.recv())
            .await
            .expect("the latest token must take over without ambiguity")
            .unwrap();
        assert_eq!(
            path.punch_socket.claim_authorized_peer(source),
            Some((remote.node_id(), 2))
        );

        let verified = AttemptProgress::default();
        assert!(verified.begin_transport());
        assert!(
            !newer_offer_can_supersede(
                Some(&PeerLifecycle::Authenticating),
                old_token,
                new_token,
                &verified,
            ),
            "verified outbound QUIC must be preserved for both roles"
        );
        assert!(!newer_offer_can_supersede(
            Some(&PeerLifecycle::Negotiating),
            old_token,
            new_token,
            &AttemptProgress::default(),
        ));
        assert!(!newer_offer_can_supersede(
            Some(&PeerLifecycle::Connected(RemoteAuthorization::default())),
            old_token,
            new_token,
            &AttemptProgress::default(),
        ));

        drop(new_receiver);
        network.close();
        network.wait_idle().await;
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
    let healthy_peer = Identity::generate().node_id();
    let mut connections = HashMap::from([(healthy_peer, 1)]);
    let mut attempts = HashMap::new();
    release_unused_recovery_networks(&mut networks, &connections, &attempts, &pending).await;
    assert_eq!(
        networks.len(),
        1,
        "an outstanding lookup still owns its socket"
    );
    pending.remove(&peer);
    connections.insert(peer, 2);
    release_unused_recovery_networks(&mut networks, &connections, &attempts, &pending).await;
    assert_eq!(
        networks.len(),
        1,
        "a recovery route winner must retain its socket until replaced"
    );
    connections.insert(peer, 1);
    attempts.insert((peer, 1), 2);
    release_unused_recovery_networks(&mut networks, &connections, &attempts, &pending).await;
    assert_eq!(
        networks.len(),
        1,
        "an active recovery attempt must retain its socket"
    );
    attempts.insert((peer, 1), 1);
    release_unused_recovery_networks(&mut networks, &connections, &attempts, &pending).await;
    assert!(networks.is_empty());
    assert_eq!(connections.get(&peer), Some(&1));
    assert_eq!(connections.get(&healthy_peer), Some(&1));
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
        assert!(
            inspect(&hc).await[&a.node_id()]
                .1
                .remote_address()
                .is_ipv4()
        );
        assert!(ticks.load(Ordering::SeqCst) > 20);
        ha.disconnect_peer(c.node_id()).await.unwrap();
        assert!(healthy_a.close_reason().is_none());
        assert!(healthy_b.close_reason().is_none());
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
