use super::*;

async fn configured_pair(sandbox: &Sandbox, discovery: bool) -> (Node, Node) {
    let mut a = Node::init(sandbox.0.join("direct-a"));
    let mut b = Node::init(sandbox.0.join("direct-b"));
    for (node, peer) in [(&a, b.id()), (&b, a.id())] {
        let mut settings = ConfigRecord::load(&node.root.join("node.json"))
            .unwrap()
            .settings
            .redacted();
        assert_eq!(settings["signal_server"], "");
        settings["direct"]["lan_discovery"] = json!(discovery);
        grant(&mut settings, &[peer]);
        offline(node, settings);
    }
    a.start().await;
    b.start().await;
    for node in [&a, &b] {
        node.ok(Api::SetPassword {
            expected_revision: 1,
            password: "X7t9Q2k4".into(),
        })
        .await;
    }
    (a, b)
}
async fn ipv4_listener(node: &Node) -> std::net::SocketAddr {
    let until = Instant::now() + Duration::from_secs(10);
    loop {
        let value = node.ok(Api::Nearby).await;
        if let Some(a) = value["listen"].as_array().and_then(|all| {
            all.iter()
                .filter_map(Value::as_str)
                .filter_map(|s| s.parse::<std::net::SocketAddr>().ok())
                .find(|a| a.is_ipv4() && a.ip().is_loopback())
        }) {
            return a;
        }
        assert!(Instant::now() < until, "no direct listener: {value}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn no_signal_agent_transfers_files_and_tcp_then_enforces_revocation() {
    let sandbox = Sandbox::new();
    let (mut a, mut b) = configured_pair(&sandbox, false).await;
    let a_id = a.id();
    let b_id = b.id();
    let key_a = fs::read(a.root.join("identity.key")).unwrap();
    let key_b = fs::read(b.root.join("identity.key")).unwrap();
    let target = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let target_addr = target.local_addr().unwrap();
    let echo = tokio::spawn(async move {
        while let Ok((mut stream, _)) = target.accept().await {
            tokio::spawn(async move {
                let (mut r, mut w) = stream.split();
                let _ = tokio::io::copy(&mut r, &mut w).await;
            });
        }
    });
    let mut config = b.config().await;
    config["settings"]["services"] = json!([{"id":"echo","name":"Echo","target":target_addr,"allowed_peers":[a_id],"enabled":true}]);
    assert!(b.apply(&config).await.error.is_none());
    let endpoint = ipv4_listener(&b).await;
    // Exercise the shipped CLI, pinning the existing remote identity.
    cli_ok(&a.root, &["connect-address", &b_id, &endpoint.to_string()]);
    wait_peer(&a, &b_id).await;
    wait_peer(&b, &a_id).await;
    assert_eq!(a.ok(Api::Status).await["node"]["signal"], "disabled");
    assert_eq!(b.ok(Api::Status).await["node"]["signal"], "disabled");
    assert_eq!(
        a.ok(Api::Services { peer: b_id.clone() }).await[0]["id"],
        "echo"
    );
    let port = free_tcp();
    let mut config = a.config().await;
    config["settings"]["forwards"] = json!([forward("echo", &b_id, port)]);
    config["settings"]["service_ids"] = json!({"echo":"echo"});
    config["settings"]["direct"]["peers"][&b_id] = json!([endpoint]);
    assert!(a.apply(&config).await.error.is_none());
    echo_request(port, b"signal-free authorized TCP").await;
    let bytes = vec![0x6du8; 256_123];
    let source = a.root.join("direct-payload.bin");
    fs::write(&source, &bytes).unwrap();
    cli_ok(&a.root, &["send", &b_id, source.to_str().unwrap()]);
    let until = Instant::now() + Duration::from_secs(30);
    loop {
        let sent = a.ok(Api::Tasks { offset: 0 }).await;
        let received = b.ok(Api::Tasks { offset: 0 }).await;
        let complete = |v: &Value| {
            v["tasks"]
                .as_array()
                .unwrap()
                .iter()
                .any(|t| t["state"] == "completed")
        };
        if complete(&sent) && complete(&received) {
            break;
        }
        assert!(
            Instant::now() < until,
            "file incomplete: {sent} / {received}"
        );
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    assert_eq!(
        blake3::hash(&fs::read(b.root.join("receive/direct-payload.bin")).unwrap()),
        blake3::hash(&bytes)
    );
    // Restart only this disposable fixture. Saved direct addresses and identity survive.
    a.stop().await;
    a.start().await;
    echo_request(port, b"persisted route after fixture restart").await;
    wait_peer(&a, &b_id).await;
    assert_eq!(fs::read(a.root.join("identity.key")).unwrap(), key_a);
    assert_eq!(fs::read(b.root.join("identity.key")).unwrap(), key_b);
    let mut revoked = b.config().await;
    revoked["settings"]["resources"]["peers"][0]["disabled"] = json!(true);
    assert!(b.apply(&revoked).await.error.is_none());
    let until = Instant::now() + Duration::from_secs(10);
    loop {
        if a.response(Api::Services { peer: b_id.clone() })
            .await
            .error
            .is_some()
        {
            break;
        }
        assert!(
            Instant::now() < until,
            "revoked peer still has service access"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    a.stop().await;
    b.stop().await;
    echo.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn no_signal_agents_discover_via_mdns_but_only_connect_after_explicit_request() {
    let sandbox = Sandbox::new();
    let (mut a, mut b) = configured_pair(&sandbox, true).await;
    let b_id = b.id();
    let until = Instant::now() + Duration::from_secs(25);
    loop {
        let value = a.ok(Api::Nearby).await;
        assert_eq!(value["discovery"], "active");
        if value["nearby"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["node_id"] == b_id)
        {
            break;
        }
        assert!(Instant::now() < until, "mDNS peer not found: {value}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        a.ok(Api::Status).await["node"]["peers"]
            .as_object()
            .unwrap()
            .is_empty(),
        "discovery must not connect automatically"
    );
    // No IP is supplied: the Agent resolves this full identity from mDNS hints.
    a.ok(Api::Connect {
        peer: b_id.clone(),
        password: None,
    })
    .await;
    wait_peer(&a, &b_id).await;
    assert_eq!(a.ok(Api::Status).await["node"]["signal"], "disabled");
    a.stop().await;
    b.stop().await;
}

// SIGSTOP/SIGCONT exercises the real wall/monotonic gap detector without
// suspending the developer's Mac or restarting either Agent process.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn wake_refreshes_candidates_and_restores_mapping_in_the_same_agent_process() {
    let sandbox = Sandbox::new();
    let (mut a, mut b) = configured_pair(&sandbox, false).await;
    let target = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let target_addr = target.local_addr().unwrap();
    let echo = tokio::spawn(async move {
        while let Ok((mut stream, _)) = target.accept().await {
            tokio::spawn(async move {
                let (mut r, mut w) = stream.split();
                let _ = tokio::io::copy(&mut r, &mut w).await;
            });
        }
    });
    let port = free_tcp();
    let mut config = b.config().await;
    config["settings"]["services"] = json!([{"id":"echo","name":"Echo","target":target_addr,"allowed_peers":[a.id()],"enabled":true}]);
    assert!(b.apply(&config).await.error.is_none());
    let endpoint = ipv4_listener(&b).await;
    let mut config = a.config().await;
    config["settings"]["forwards"] = json!([forward("echo", &b.id(), port)]);
    config["settings"]["service_ids"] = json!({"echo":"echo"});
    config["settings"]["direct"]["peers"][b.id()] = json!([endpoint]);
    assert!(a.apply(&config).await.error.is_none());
    // No explicit connect: an active mapping must drive its own connection.
    wait_peer(&a, &b.id()).await;
    echo_request(port, b"before wake").await;
    let before = a.ok(Api::Status).await;
    let old_listen = ipv4_listener(&a).await;
    let files = ["node.json", "identity.key", "tasks.json"]
        .map(|name| (name, fs::read(a.root.join(name)).unwrap()));
    let pid = a.process.as_ref().unwrap().0.id();
    struct Resume(u32);
    impl Drop for Resume {
        fn drop(&mut self) {
            let _ = Command::new("kill")
                .args(["-CONT", &self.0.to_string()])
                .status();
        }
    }
    let resume = Resume(pid);
    assert!(
        Command::new("kill")
            .args(["-STOP", &pid.to_string()])
            .status()
            .unwrap()
            .success()
    );
    tokio::time::sleep(Duration::from_secs(32)).await;
    drop(resume);
    let until = Instant::now() + Duration::from_secs(30);
    loop {
        if ipv4_listener(&a).await != old_listen {
            break;
        }
        assert!(Instant::now() < until, "wake did not refresh UDP endpoints");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    wait_peer(&a, &b.id()).await;
    echo_request(port, b"after wake without restarting Agent").await;
    let after = a.ok(Api::Status).await;
    assert_eq!(before["epoch"], after["epoch"]);
    assert_eq!(a.process.as_ref().unwrap().0.id(), pid);
    assert!(a.process.as_mut().unwrap().0.try_wait().unwrap().is_none());
    assert!(
        after["node"]["peers"][b.id()]["generation"]
            .as_u64()
            .unwrap()
            > before["node"]["peers"][b.id()]["generation"]
                .as_u64()
                .unwrap()
    );
    for (name, bytes) in files {
        assert_eq!(fs::read(a.root.join(name)).unwrap(), bytes, "{name}");
    }
    a.stop().await;
    b.stop().await;
    echo.abort();
}
