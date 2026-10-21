//! Real signal-free QUIC connections; no signaling/STUN/relay fixtures exist here.
use super::*;

fn direct_config(family: crate::net::NetworkFamilies) -> NodeSessionConfig {
    let mut c = NodeSessionConfig::new("");
    c.direct.enabled = true;
    c.direct.lan_discovery = false;
    c.remote_auth = Some(test_verifier());
    c.network.families = family;
    c.network.include_loopback = true;
    // These would fail if signal-free startup accidentally tried external probing.
    c.network.stun_servers = vec!["invalid.invalid:3478".into()];
    c
}
async fn address(handle: &NodeSessionHandle, ipv6: bool) -> SocketAddr {
    time::timeout(Duration::from_secs(5), async {
        loop {
            let s = handle.snapshot().unwrap();
            if let Some(a) = s
                .connectivity
                .listen
                .into_iter()
                .find(|a| a.is_ipv6() == ipv6 && a.ip().is_loopback())
            {
                return a;
            }
            time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("local direct endpoint not published")
}
async fn connected(handle: &NodeSessionHandle, peer: NodeId) {
    time::timeout(Duration::from_secs(8), async {
        loop {
            let s = handle.snapshot().unwrap();
            assert_ne!(s.signal, "online", "test must never use signaling");
            if s.peers
                .get(&peer.to_hex())
                .is_some_and(|p| p.phase == "connected" && p.outbound_password)
            {
                return;
            }
            time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("signal-free peer did not authenticate")
}
async fn direct_case(ipv6: bool, caller_is_lower: bool) {
    if ipv6 {
        assert!(
            crate::net::family::ipv6_test_available(),
            "IPv6 required for this regression"
        );
    }
    let server = Identity::generate();
    let mut caller = Identity::generate();
    while should_initiate_quic(caller.node_id(), server.node_id()) != caller_is_lower {
        caller = Identity::generate();
    }
    let family = if ipv6 {
        crate::net::NetworkFamilies::Ipv6Only
    } else {
        crate::net::NetworkFamilies::Ipv4Only
    };
    let mut c = direct_config(family);
    let target = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut service = crate::node::core::config::AllowedForwardTarget::new(
        "fixture",
        target.local_addr().unwrap(),
        vec![caller.node_id().to_hex()],
    );
    service.id = "fixture".into();
    c.allowed_forward_targets.push(service);
    let (remote, _remote_events) = spawn(server.clone(), c).unwrap();
    let (local, _local_events) = spawn(caller, direct_config(family)).unwrap();
    let endpoint = address(&remote, ipv6).await;
    local
        .connect_address(server.node_id(), vec![endpoint], Some(test_password()))
        .await
        .unwrap();
    connected(&local, server.node_id()).await;
    let services = local.list_services(server.node_id()).await.unwrap();
    assert_eq!(services.len(), 1);
    assert_eq!(services[0].id, "fixture");
    // A supplied password only authorizes this direction, exactly as with signaling.
    assert!(
        remote
            .list_services(NodeId::from_hex(&local.snapshot().unwrap().node_id).unwrap())
            .await
            .is_err()
    );
    local.shutdown();
    remote.shutdown();
}
#[tokio::test]
async fn direct_ipv4_caller_can_have_either_application_role() {
    direct_case(false, true).await;
    direct_case(false, false).await;
}
#[tokio::test]
async fn direct_ipv6_without_signal_or_stun_retains_directional_service_auth() {
    direct_case(true, true).await;
    direct_case(true, false).await;
}
#[tokio::test]
async fn wrong_expected_identity_never_publishes_a_peer_or_service() {
    let server = Identity::generate();
    let wrong = Identity::generate();
    let (remote, _events) =
        spawn(server, direct_config(crate::net::NetworkFamilies::Ipv4Only)).unwrap();
    let (local, _events2) = spawn(
        Identity::generate(),
        direct_config(crate::net::NetworkFamilies::Ipv4Only),
    )
    .unwrap();
    local
        .connect_address(
            wrong.node_id(),
            vec![address(&remote, false).await],
            Some(test_password()),
        )
        .await
        .unwrap();
    time::timeout(Duration::from_secs(8), async {
        loop {
            let s = local.snapshot().unwrap();
            if s.peers
                .get(&wrong.node_id().to_hex())
                .is_some_and(|p| p.phase == "failed")
            {
                break;
            }
            time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(local.list_services(wrong.node_id()).await.is_err());
    assert!(
        remote
            .snapshot()
            .unwrap()
            .peers
            .values()
            .all(|p| p.phase != "connected")
    );
    local.shutdown();
    remote.shutdown();
}
#[tokio::test]
async fn wrong_password_cannot_open_services_and_valid_retry_uses_fresh_connection() {
    let server = Identity::generate();
    let (remote, _events) = spawn(
        server.clone(),
        direct_config(crate::net::NetworkFamilies::Ipv4Only),
    )
    .unwrap();
    let (local, _events2) = spawn(
        Identity::generate(),
        direct_config(crate::net::NetworkFamilies::Ipv4Only),
    )
    .unwrap();
    let endpoint = address(&remote, false).await;
    local
        .connect_address(
            server.node_id(),
            vec![endpoint],
            Some(SecretPassword::new("Wrong9Pass".into()).unwrap()),
        )
        .await
        .unwrap();
    time::timeout(Duration::from_secs(8), async {
        loop {
            let s = local.snapshot().unwrap();
            if s.peers
                .get(&server.node_id().to_hex())
                .is_some_and(|p| p.phase == "failed")
            {
                break;
            }
            time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(local.list_services(server.node_id()).await.is_err());
    local
        .connect_address(server.node_id(), vec![endpoint], Some(test_password()))
        .await
        .unwrap();
    connected(&local, server.node_id()).await;
    local.shutdown();
    remote.shutdown();
}

#[tokio::test]
async fn simultaneous_signal_free_dials_publish_one_authorized_session() {
    let a = Identity::generate();
    let b = Identity::generate();
    let a_id = a.node_id();
    let b_id = b.node_id();
    let (ha, _ea) = spawn(a, direct_config(crate::net::NetworkFamilies::Ipv4Only)).unwrap();
    let (hb, _eb) = spawn(b, direct_config(crate::net::NetworkFamilies::Ipv4Only)).unwrap();
    let aa = address(&ha, false).await;
    let ba = address(&hb, false).await;
    let (ra, rb) = tokio::join!(
        ha.connect_address(b_id, vec![ba], Some(test_password())),
        hb.connect_address(a_id, vec![aa], Some(test_password()))
    );
    ra.unwrap();
    rb.unwrap();
    connected(&ha, b_id).await;
    connected(&hb, a_id).await;
    assert_eq!(ha.snapshot().unwrap().peers.len(), 1);
    assert_eq!(hb.snapshot().unwrap().peers.len(), 1);
    ha.shutdown();
    hb.shutdown();
}

#[tokio::test]
async fn signal_free_start_does_not_contact_configured_stun() {
    let spy = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let mut config = direct_config(crate::net::NetworkFamilies::Ipv4Only);
    config.network.stun_servers = vec![spy.local_addr().unwrap().to_string()];
    config.network.relay_server = Some("relay.invalid:9999".into());
    let (node, _events) = spawn(Identity::generate(), config).unwrap();
    address(&node, false).await;
    assert_eq!(node.snapshot().unwrap().signal, "disabled");
    let mut packet = [0; 512];
    assert!(
        time::timeout(Duration::from_millis(250), spy.recv_from(&mut packet))
            .await
            .is_err(),
        "signal-free mode must not probe STUN"
    );
    node.shutdown();
}
