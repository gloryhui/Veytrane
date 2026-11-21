use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TestAgentMix {
    FullBoth,
    MinimalBoth,
    MinimalClientFullServer,
    TcpClientFullServer,
    BareClientFullServer,
}

fn minimal_agent() -> Option<String> {
    std::env::var_os("VEYTRANE_TEST_MINIMAL_AGENT")
        .map(|path| path.into_string().expect("验收二进制路径不是 UTF-8"))
}

fn test_agent_mix(minimal: Option<&str>) -> TestAgentMix {
    match std::env::var("VEYTRANE_TEST_AGENT_MIX").ok().as_deref() {
        Some("full-both") => TestAgentMix::FullBoth,
        Some("minimal-both") => {
            assert!(
                minimal.is_some(),
                "minimal-both requires VEYTRANE_TEST_MINIMAL_AGENT"
            );
            TestAgentMix::MinimalBoth
        }
        Some("minimal-client-full-server") => {
            assert!(
                minimal.is_some(),
                "minimal-client-full-server requires VEYTRANE_TEST_MINIMAL_AGENT"
            );
            TestAgentMix::MinimalClientFullServer
        }
        Some("tcp-client-full-server") => {
            assert!(
                minimal.is_some(),
                "tcp-client-full-server requires VEYTRANE_TEST_MINIMAL_AGENT"
            );
            TestAgentMix::TcpClientFullServer
        }
        Some("bare-client-full-server") => {
            assert!(
                minimal.is_some(),
                "bare-client-full-server requires VEYTRANE_TEST_MINIMAL_AGENT"
            );
            TestAgentMix::BareClientFullServer
        }
        Some(other) => panic!("unknown VEYTRANE_TEST_AGENT_MIX={other}"),
        None if minimal.is_some() => TestAgentMix::MinimalBoth,
        None => TestAgentMix::FullBoth,
    }
}

fn init_agent(root: PathBuf, binary: Option<&str>) -> Node {
    let Some(binary) = binary else {
        return Node::init(root);
    };
    let initialized = Command::new(binary)
        .arg("--data-dir")
        .arg(&root)
        .arg("--init")
        .output()
        .unwrap();
    assert!(
        initialized.status.success(),
        "{}",
        String::from_utf8_lossy(&initialized.stderr)
    );
    let record = ConfigRecord::load(&root.join("node.json")).unwrap();
    assert!(!record.settings.enable_files);
    Node::open(root)
}

async fn start_agent(node: &mut Node, binary: Option<&str>) {
    if let Some(binary) = binary {
        node.start_with(binary).await;
    } else {
        node.start().await;
    }
}

async fn configured_pair(sandbox: &Sandbox, discovery: bool) -> (Node, Node) {
    configured_pair_with_agent(sandbox, discovery, false, None).await
}

async fn configured_pair_with_agent(
    sandbox: &Sandbox,
    discovery: bool,
    files: bool,
    binary: Option<&str>,
) -> (Node, Node) {
    let mut a = Node::init(sandbox.0.join("direct-a"));
    let mut b = Node::init(sandbox.0.join("direct-b"));
    for (node, peer) in [(&a, b.id()), (&b, a.id())] {
        let mut settings = ConfigRecord::load(&node.root.join("node.json"))
            .unwrap()
            .settings
            .redacted();
        assert_eq!(settings["signal_server"], "");
        settings["direct"]["lan_discovery"] = json!(discovery);
        settings["enable_files"] = json!(files);
        grant(&mut settings, &[peer]);
        offline(node, settings);
    }
    start_agent(&mut a, binary).await;
    start_agent(&mut b, binary).await;
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
async fn file_disabled_agents_keep_speed_tcp_and_reconnect_without_file_storage() {
    use p2p_file::node::api::protocol::SpeedtestDirection;

    let sandbox = Sandbox::new();
    let minimal = minimal_agent();
    let mix = test_agent_mix(minimal.as_deref());
    let (client_binary, server_binary) = match mix {
        TestAgentMix::FullBoth => (None, None),
        TestAgentMix::MinimalBoth => (minimal.as_deref(), minimal.as_deref()),
        TestAgentMix::MinimalClientFullServer
        | TestAgentMix::TcpClientFullServer
        | TestAgentMix::BareClientFullServer => {
            (minimal.as_deref(), None)
        }
    };
    let server_files = matches!(
        mix,
        TestAgentMix::MinimalClientFullServer
            | TestAgentMix::TcpClientFullServer
            | TestAgentMix::BareClientFullServer
    );
    let client_speed = matches!(
        mix,
        TestAgentMix::FullBoth | TestAgentMix::MinimalBoth | TestAgentMix::MinimalClientFullServer
    );
    let client_tcp = mix != TestAgentMix::BareClientFullServer;
    let mut client = init_agent(sandbox.0.join("client"), client_binary);
    let mut server = init_agent(sandbox.0.join("server"), server_binary);
    let client_id = client.id();
    let server_id = server.id();
    let target = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let target_addr = target.local_addr().unwrap();
    let listen = free_tcp();
    let echo = tokio::spawn(async move {
        while let Ok((mut stream, _)) = target.accept().await {
            tokio::spawn(async move {
                let (mut reader, mut writer) = stream.split();
                let _ = tokio::io::copy(&mut reader, &mut writer).await;
            });
        }
    });
    for (node, peer) in [(&client, &server_id), (&server, &client_id)] {
        let mut settings = ConfigRecord::load(&node.root.join("node.json"))
            .unwrap()
            .settings
            .redacted();
        let files_enabled = node.root == server.root && server_files;
        settings["enable_files"] = json!(files_enabled);
        settings["enable_speed"] = json!(node.root == server.root || client_speed);
        settings["enable_tcp"] = json!(node.root == server.root || client_tcp);
        settings["direct"]["lan_discovery"] = json!(false);
        grant(&mut settings, std::slice::from_ref(peer));
        settings["resources"]["peers"][0]["files"] = json!(files_enabled);
        if node.root == client.root {
            settings["forwards"] = json!([forward("echo", peer, listen)]);
            settings["service_ids"] = json!({"echo": "echo"});
        } else {
            settings["services"] = json!([
                {"id":"echo","name":"Echo","target":target_addr,"allowed_peers":[peer],"enabled":true}
            ]);
        }
        offline(node, settings);
    }

    start_agent(&mut server, server_binary).await;
    start_agent(&mut client, client_binary).await;
    let client_status = client.ok(Api::Status).await;
    assert_eq!(
        client_status["capabilities"],
        json!({"files": false, "speed": client_speed, "tcp": client_tcp})
    );
    assert_eq!(client_status["files_compiled"], client_binary.is_none());
    assert_eq!(client_status["speeds"], json!([]));
    assert_eq!(
        server.ok(Api::Status).await["capabilities"],
        json!({"files": server_files, "speed": true, "tcp": true})
    );
    if server_files {
        let until = Instant::now() + Duration::from_secs(10);
        while !(server.root.join("tasks.json").is_file() && server.root.join("receive").is_dir()) {
            assert!(
                Instant::now() < until,
                "full server did not initialize its file store"
            );
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
        assert!(
            !client.root.join("tasks.json").exists(),
            "minimal client created a task store"
        );
    }
    if client_binary.is_some() {
        let before = fs::read(client.root.join("node.json")).unwrap();
        let mut config = client.config().await;
        config["settings"]["enable_files"] = json!(true);
        let rejected = client.apply(&config).await;
        assert_eq!(rejected.error.unwrap().code, "capability_disabled");
        assert_eq!(fs::read(client.root.join("node.json")).unwrap(), before);

        let mut config = client.config().await;
        config["settings"]["file_limits"]["upload_kib"] = json!(64);
        let rejected = client.apply(&config).await;
        assert_eq!(rejected.error.unwrap().code, "capability_disabled");
        assert_eq!(fs::read(client.root.join("node.json")).unwrap(), before);
    }
    for node in [&client, &server] {
        node.ok(Api::SetPassword {
            expected_revision: 1,
            password: "Test9Speed".into(),
        })
        .await;
    }
    let endpoint = ipv4_listener(&server).await;
    cli_ok(
        &client.root,
        &["connect-address", &server_id, &endpoint.to_string()],
    );
    wait_peer(&client, &server_id).await;
    wait_peer(&server, &client_id).await;
    if client_tcp {
        echo_request(listen, b"TCP before speed").await;
    } else {
        assert!(
            TcpListener::bind(listen).is_ok(),
            "disabled TCP opened its saved listener"
        );
        let rejected = client
            .response(Api::StartForward {
                rule: "echo".into(),
            })
            .await;
        assert_eq!(rejected.error.unwrap().code, "capability_disabled");
        for (node, peer) in [(&client, &server_id), (&server, &client_id)] {
            assert!(
                node.response(Api::Services { peer: peer.clone() })
                    .await
                    .error
                    .is_some()
            );
        }
    }
    let rejected = client
        .response(file_request(FileApi::Send {
            peer: server_id.clone(),
            paths: vec![client.root.join("not-a-file")],
        }))
        .await;
    assert_eq!(rejected.error.unwrap().code, "capability_disabled");
    let speed = Api::StartSpeed {
        peer: server_id.clone(),
        direction: SpeedtestDirection::Upload,
        seconds: 30,
    };
    if client_speed {
        client.ok(speed).await;
        let until = Instant::now() + Duration::from_secs(45);
        let mut tcp_during_speed = false;
        loop {
            let tasks = client.ok(Api::Tasks { offset: 0 }).await;
            assert_eq!(tasks["tasks"], json!([]));
            assert!(tasks["queue"].is_null());
            assert_eq!(tasks["directory_previews"], json!([]));
            let speeds = tasks["speeds"].as_array().unwrap();
            if !tcp_during_speed
                && speeds
                    .iter()
                    .any(|row| row["peer"] == server_id && row["state"] == "Running")
            {
                echo_request(listen, b"TCP during speed").await;
                tcp_during_speed = true;
            }
            if let Some(result) = speeds.iter().find(|row| {
                row["peer"] == server_id
                    && row["state"] == "Completed"
                    && row["bytes"].as_u64().is_some_and(|bytes| bytes > 0)
            }) {
                assert!(
                    client.ok(Api::Status).await["speeds"]
                        .as_array()
                        .unwrap()
                        .contains(result)
                );
                break;
            }
            assert!(Instant::now() < until, "speed did not complete: {tasks}");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(tcp_during_speed);
    } else {
        let rejected = client.response(speed).await;
        assert_eq!(rejected.error.unwrap().code, "capability_disabled");
        assert_eq!(client.ok(Api::Tasks { offset: 0 }).await["speeds"], json!([]));
    }

    client
        .ok(Api::Disconnect {
            peer: server_id.clone(),
        })
        .await;
    cli_ok(
        &client.root,
        &["connect-address", &server_id, &endpoint.to_string()],
    );
    wait_peer(&client, &server_id).await;
    if client_tcp {
        echo_request(listen, b"TCP after reconnect").await;
    }
    client.stop().await;
    server.stop().await;
    for (node, has_files) in [(&client, false), (&server, server_files)] {
        for relative in ["receive", "tasks.json", "tasks.lock"] {
            assert_eq!(
                node.root.join(relative).exists(),
                has_files,
                "unexpected {relative} state"
            );
        }
    }
    echo.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn completed_file_task_survives_files_disabled_then_full_reenable() {
    use p2p_file::node::{core::task_model::TaskState, core::task_store::TaskStore};

    let sandbox = Sandbox::new();
    let baseline = std::env::var("VEYTRANE_TEST_BASELINE_AGENT").ok();
    let (mut sender, mut receiver) =
        configured_pair_with_agent(&sandbox, false, true, baseline.as_deref()).await;
    let sender_id = sender.id();
    let receiver_id = receiver.id();
    let bytes = vec![0x6bu8; 192 * 1024];
    let source = sender.root.join("retained-before-disable.bin");
    let received = receiver.root.join("receive/retained-before-disable.bin");
    fs::write(&source, &bytes).unwrap();
    let endpoint = ipv4_listener(&receiver).await;
    cli_ok(
        &sender.root,
        &["connect-address", &receiver_id, &endpoint.to_string()],
    );
    wait_peer(&sender, &receiver_id).await;
    wait_peer(&receiver, &sender_id).await;
    cli_ok(
        &sender.root,
        &["send", &receiver_id, source.to_str().unwrap()],
    );
    let until = Instant::now() + Duration::from_secs(45);
    let task_id = loop {
        let tasks = receiver.ok(Api::Tasks { offset: 0 }).await;
        if let Some(id) = tasks["tasks"].as_array().and_then(|rows| {
            rows.iter()
                .find(|row| row["state"] == "completed")
                .and_then(|row| row["id"].as_str())
        }) {
            break id.to_owned();
        }
        assert!(Instant::now() < until, "file did not complete: {tasks}");
        tokio::time::sleep(Duration::from_millis(30)).await;
    };
    assert_eq!(
        blake3::hash(&fs::read(&received).unwrap()),
        blake3::hash(&bytes)
    );
    sender.stop().await;
    receiver.stop().await;

    let tasks_path = receiver.root.join("tasks.json");
    let (store, _) = TaskStore::open(&tasks_path).unwrap();
    assert!(store.list().iter().any(|task| {
        task.task_id().to_string() == task_id && task.state() == TaskState::Completed
    }));
    drop(store);
    let persisted_tasks = fs::read(&tasks_path).unwrap();

    let mut disabled = ConfigRecord::load(&receiver.root.join("node.json"))
        .unwrap()
        .settings
        .redacted();
    disabled["enable_files"] = json!(false);
    offline(&receiver, disabled);
    let minimal = minimal_agent();
    start_agent(&mut receiver, minimal.as_deref()).await;
    let hidden = receiver.ok(Api::Tasks { offset: 0 }).await;
    assert_eq!(hidden["tasks"], json!([]));
    assert_eq!(fs::read(&tasks_path).unwrap(), persisted_tasks);
    assert!(received.is_file());
    receiver.stop().await;
    assert_eq!(fs::read(&tasks_path).unwrap(), persisted_tasks);

    let mut enabled = ConfigRecord::load(&receiver.root.join("node.json"))
        .unwrap()
        .settings
        .redacted();
    enabled["enable_files"] = json!(true);
    offline(&receiver, enabled);
    receiver.start().await;
    let restored = receiver.ok(Api::Tasks { offset: 0 }).await;
    assert!(restored["tasks"].as_array().unwrap().iter().any(|task| {
        task["id"].as_str() == Some(task_id.as_str()) && task["state"] == "completed"
    }));
    receiver.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn no_signal_agent_transfers_files_and_tcp_then_enforces_revocation() {
    let sandbox = Sandbox::new();
    let (mut a, mut b) = configured_pair_with_agent(&sandbox, false, true, None).await;
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
    let (mut a, mut b) = configured_pair_with_agent(&sandbox, false, true, None).await;
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
