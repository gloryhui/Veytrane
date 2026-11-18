use super::*;

#[tokio::test]
async fn port_restricted_stale_primary_recovers_with_republished_socket_and_keeps_tcp() {
    for local_initiates in [true, false] {
        let (signal, server) = start_local_server().await;
        let a = Identity::generate();
        let b = Identity::generate();
        let mut c = Identity::generate();
        while should_initiate_quic(a.node_id(), c.node_id()) != local_initiates {
            c = Identity::generate();
        }

        // A's original mapping works for B, but packets between it and C always
        // drop. The recovery mapping is subject to reciprocal port filtering.
        let (networks, public, allowed, stats, _proxies) =
            restricted_nat_harness_blocking(&[(0, 2), (2, 0)]).await;
        let [mut primary, mut healthy, mut remote, mut recovery]: [DesktopNetwork; 4] =
            networks.try_into().ok().unwrap();
        let primary_private = loopback_address(primary.paths[0].local_addr);
        let remote_private = loopback_address(remote.paths[0].local_addr);
        let recovery_private = loopback_address(recovery.paths[0].local_addr);
        let primary_public = public[&primary_private];
        let remote_public = public[&remote_private];
        let recovery_public = public[&recovery_private];
        for network in [&mut primary, &mut healthy, &mut remote, &mut recovery] {
            let address = public[&loopback_address(network.paths[0].local_addr)];
            let candidates = vec![Candidate::reflexive(address)];
            network.paths[0].local_candidates = candidates.clone();
            network.local_candidates = candidates;
        }

        let echo = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let target = echo.local_addr().unwrap();
        let echo_task = tokio::spawn(async move {
            // Exactly one backend stream is used. Recovery must not open a new one.
            let (mut stream, _) = echo.accept().await.unwrap();
            let (mut r, mut w) = stream.split();
            tokio::io::copy(&mut r, &mut w).await.unwrap();
        });
        let reserve = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let listen = reserve.local_addr().unwrap();
        drop(reserve);
        let mut rule = crate::node::core::config::TunnelRule::new(
            "healthy-while-republishing-candidates",
            b.node_id().to_hex(),
            listen.port(),
            target,
        );
        rule.auto_start = true;
        let mut ca = local_config(signal);
        ca.test_prepared_networks = TestPreparedNetworks::new([primary, recovery]);
        ca.tunnel_rules.push(rule.clone());
        let mut cb = local_config(signal);
        cb.test_prepared_networks = TestPreparedNetworks::new([healthy]);
        cb.allowed_forward_targets
            .push(service_grant("echo", target, vec![a.node_id().to_hex()]));
        let mut cc = local_config(signal);
        cc.test_prepared_networks = TestPreparedNetworks::new([remote]);
        // Keep C's advertised port fixed so its own rotation cannot repair the fixture.
        cc.network.local_port = remote_private.port();
        cc.network.advertise_only = true;

        let (hb, mut eb) = spawn(b.clone(), cb).unwrap();
        wait_signal_online(&mut eb).await;
        let (hc, mut ec) = spawn(c.clone(), cc).unwrap();
        wait_signal_online(&mut ec).await;
        // Give C a healthy peer as well, so its independent recovery policy
        // cannot globally refresh the test topology after its own failures.
        hc.connect_peer(b.node_id()).unwrap();
        wait_connected(&mut ec, &[b.node_id()]).await;
        let (ha, mut ea) = spawn(a.clone(), ca).unwrap();
        wait_specific_tunnel_state(&mut ea, &rule.id, TunnelRuntimeState::Running).await;
        let healthy_a = inspect(&ha).await[&b.node_id()].1.clone();
        let healthy_b = inspect(&hb).await[&a.node_id()].1.clone();

        let mut stream = tokio::net::TcpStream::connect(rule.listen).await.unwrap();
        let ticks = Arc::new(AtomicUsize::new(0));
        let tick_count = ticks.clone();
        let (stop, mut stopping) = watch::channel(false);
        let (first_echo, first) = oneshot::channel();
        let heartbeat = tokio::spawn(async move {
            let mut first_echo = Some(first_echo);
            loop {
                let value = tick_count.load(Ordering::SeqCst).to_le_bytes();
                time::timeout(Duration::from_secs(3), async {
                    stream.write_all(&value).await.unwrap();
                    let mut received = [0; std::mem::size_of::<usize>()];
                    stream.read_exact(&mut received).await.unwrap();
                    assert_eq!(received, value);
                })
                .await
                .expect("the same TCP stream must echo throughout candidate recovery");
                tick_count.fetch_add(1, Ordering::SeqCst);
                if let Some(first_echo) = first_echo.take() {
                    let _ = first_echo.send(());
                }
                tokio::select! {
                    _ = time::sleep(Duration::from_millis(25)) => {},
                    _ = stopping.changed() => return,
                }
            }
        });
        time::timeout(Duration::from_secs(5), first)
            .await
            .unwrap()
            .unwrap();

        ha.connect_peer(c.node_id()).unwrap();
        let mut states = Vec::new();
        let recovered = time::timeout(Duration::from_secs(150), async {
            while let Some(event) = ea.recv().await {
                if let SessionEvent::PeerState {
                    peer,
                    generation,
                    state,
                } = event
                    && peer == c.node_id()
                {
                    let connected = state.is_connected();
                    states.push((generation, state));
                    if connected {
                        return true;
                    }
                }
            }
            false
        })
        .await;
        assert!(
            matches!(recovered, Ok(true)),
            "automatic recovery must publish a reachable UDP candidate, initiator={local_initiates}, states={states:?}"
        );
        assert!(
            states
                .iter()
                .any(|(_, state)| matches!(state, PeerLifecycle::Failed(_))),
            "the old advertised mapping must fail first"
        );

        // The address seen by each Quinn peer matches the simulated NAT mapping,
        // and C actually punched the new public port to open its filter.
        assert_eq!(
            inspect(&ha).await[&c.node_id()].1.remote_address(),
            remote_public
        );
        let deadline = time::Instant::now() + Duration::from_secs(5);
        loop {
            if let Some((_, connection)) = inspect(&hc).await.get(&a.node_id()) {
                assert_eq!(connection.remote_address(), recovery_public);
                break;
            }
            assert!(time::Instant::now() < deadline, "C must authenticate A");
            time::sleep(Duration::from_millis(25)).await;
        }
        assert!(allowed.lock().unwrap()[&remote_private].contains(&recovery_public));
        assert!(
            stats.lock().unwrap()[&primary_public]
                .dropped_sources
                .contains(&remote_private)
        );
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
        let recovered_at = ticks.load(Ordering::SeqCst);
        time::timeout(Duration::from_secs(3), async {
            while ticks.load(Ordering::SeqCst) <= recovered_at {
                time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("the same TCP stream must also echo after recovery");
        assert!(recovered_at > 20);
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
