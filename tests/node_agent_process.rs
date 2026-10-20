//! Actual Agent/CLI processes and the production private IPC transport.
#![cfg(feature = "node")]
use p2p_file::{
    identity::Identity,
    node::{
        agent_client::AgentClient,
        agent_config::ConfigRecord,
        agent_paths::AgentPaths,
        ipc::{Command as Api, Request, Response},
    },
};
use serde_json::{Value, json};
use std::{
    fs,
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

struct Sandbox(PathBuf);
impl Sandbox {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("p2p-node-process-{:032x}", rand::random::<u128>()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Sandbox {
    fn drop(&mut self) {
        #[cfg(windows)]
        #[allow(clippy::permissions_set_readonly_false)] // Windows-only cleanup of synthetic read-only migration backups.
        fn writable(path: &Path) {
            if let Ok(metadata) = fs::symlink_metadata(path) {
                if metadata.file_type().is_symlink() {
                    return;
                }
                if metadata.is_dir() {
                    if let Ok(entries) = fs::read_dir(path) {
                        for entry in entries.flatten() {
                            writable(&entry.path());
                        }
                    }
                } else if metadata.permissions().readonly() {
                    let mut permissions = metadata.permissions();
                    permissions.set_readonly(false);
                    let _ = fs::set_permissions(path, permissions);
                }
            }
        }
        #[cfg(windows)]
        writable(&self.0);
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn migration_process_preserves_identity_raw_tasks_source_and_rejects_stale_preview() {
    let sandbox = Sandbox::new();
    let source = sandbox.0.join("old");
    fs::create_dir(&source).unwrap();
    let received = source.join("receive");
    fs::create_dir(&received).unwrap();
    let identity = Identity::generate();
    identity.save(&source.join("identity.key")).unwrap();
    let peer = Identity::generate().node_id().to_hex();
    let configuration = source.join("settings.json");
    let mut config = json!({"schema_version":11,"signal":null,"receive_directory":received,"send_concurrency":1,"speedtest_seconds":30,"speedtest_direction":"both","trusted_devices":[{"node_id":peer,"display_name":"legacy fixture","last_short_id":null,"trusted_at":1,"updated_at":1}]});
    fs::write(&configuration, serde_json::to_vec(&config).unwrap()).unwrap();
    use p2p_file::node::task_model::{ManifestIdentity, PeerId, TaskRecord};
    let record = TaskRecord::new_sender(
        PeerId::from_node_id(p2p_file::identity::NodeId::from_hex(&peer).unwrap()),
        source.join("cancelled.bin"),
        ManifestIdentity::blake3([7; 32], 10, 4).unwrap(),
    )
    .unwrap();
    let mut cancelled = serde_json::to_value(&record).unwrap();
    cancelled["state"] = json!("failed");
    cancelled["diagnostic"] = json!({"code":"user_cancelled","retryable":false});
    let raw =
        serde_json::to_vec_pretty(&json!({"format_version":1,"generation":42,"tasks":[cancelled]}))
            .unwrap();
    fs::write(source.join("tasks.json"), &raw).unwrap();
    let target = sandbox.0.join("imported");
    let args = [
        "migrate-preview",
        "--config-file",
        configuration.to_str().unwrap(),
        "--source-data-dir",
        source.to_str().unwrap(),
    ];
    let initial_preview = cli_ok(&target, &args);
    assert_eq!(initial_preview["result"]["tasks"], 1);
    let preview = initial_preview["result"]["preview"]
        .as_str()
        .unwrap()
        .to_owned();
    config["send_concurrency"] = json!(2);
    fs::write(&configuration, serde_json::to_vec(&config).unwrap()).unwrap();
    assert!(
        !cli(
            &target,
            &[
                "migrate-desktop",
                "--config-file",
                configuration.to_str().unwrap(),
                "--source-data-dir",
                source.to_str().unwrap(),
                "--expected-preview",
                &preview
            ]
        )
        .status
        .success()
    );
    assert!(!target.join("identity.key").exists());
    let before = fs::read(&configuration).unwrap();
    let preview = cli_ok(&target, &args)["result"]["preview"]
        .as_str()
        .unwrap()
        .to_owned();
    cli_ok(
        &target,
        &[
            "migrate-desktop",
            "--config-file",
            configuration.to_str().unwrap(),
            "--source-data-dir",
            source.to_str().unwrap(),
            "--expected-preview",
            &preview,
        ],
    );
    assert_eq!(
        Identity::load(&target.join("identity.key"))
            .unwrap()
            .node_id(),
        identity.node_id()
    );
    assert_eq!(fs::read(target.join("tasks.json")).unwrap(), raw);
    assert_eq!(fs::read(source.join("tasks.json")).unwrap(), raw);
    assert_eq!(fs::read(&configuration).unwrap(), before);
    let record = ConfigRecord::load(&target.join("node.json")).unwrap();
    assert_eq!(record.settings.send_concurrency, 2);
    assert_eq!(
        record.settings.trust_sources[&peer],
        p2p_file::node::authorization::TrustSource::LegacyImport
    );
    assert_eq!(
        record.settings.peer_modes[&peer],
        p2p_file::node::wire::PeerWireMode::LegacyDesktopV1
    );
    let key_before = fs::read(target.join("identity.key")).unwrap();
    assert!(!cli(&target, &["init"]).status.success());
    assert_eq!(fs::read(target.join("identity.key")).unwrap(), key_before);
}
struct Process(Child);
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn spawn(binary: &str, args: &[&str], log: &Path) -> Process {
    let file = fs::File::create(log).unwrap();
    Process(
        Command::new(binary)
            .args(args)
            .env("RUST_LOG", "info")
            .stdout(Stdio::from(file.try_clone().unwrap()))
            .stderr(Stdio::from(file))
            .spawn()
            .unwrap(),
    )
}
fn cli(root: &Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_p2p-node"))
        .arg("--data-dir")
        .arg(root)
        .args(args)
        .output()
        .unwrap()
}
fn cli_ok(root: &Path, args: &[&str]) -> Value {
    let result = cli(root, args);
    assert!(
        result.status.success(),
        "CLI {:?}: {} {}",
        args,
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    serde_json::from_slice(&result.stdout).unwrap()
}
struct Node {
    root: PathBuf,
    client: AgentClient,
    process: Option<Process>,
    log: PathBuf,
}
impl Node {
    fn init(root: PathBuf) -> Self {
        cli_ok(&root, &["init"]);
        let paths = AgentPaths::new(&root).unwrap();
        Self {
            log: root.join("process.log"),
            root,
            client: AgentClient::new(paths),
            process: None,
        }
    }
    fn id(&self) -> String {
        Identity::load(&self.root.join("identity.key"))
            .unwrap()
            .node_id()
            .to_hex()
    }
    async fn start(&mut self) {
        self.process = Some(spawn(
            env!("CARGO_BIN_EXE_p2p-agent"),
            &["--data-dir", self.root.to_str().unwrap()],
            &self.log,
        ));
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if self.client.execute(Api::Status).await.is_ok() {
                break;
            }
            assert!(
                self.process
                    .as_mut()
                    .unwrap()
                    .0
                    .try_wait()
                    .unwrap()
                    .is_none(),
                "agent exited: {}",
                fs::read_to_string(&self.log).unwrap()
            );
            assert!(
                Instant::now() < deadline,
                "agent not ready: {}",
                fs::read_to_string(&self.log).unwrap()
            );
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    }
    async fn response(&self, command: Api) -> Response {
        self.client.execute(command).await.unwrap()
    }
    async fn ok(&self, command: Api) -> Value {
        let response = self.response(command).await;
        assert!(
            response.error.is_none(),
            "{:?}; log: {}",
            response.error,
            fs::read_to_string(&self.log).unwrap_or_default()
        );
        response.result.unwrap()
    }
    async fn config(&self) -> Value {
        self.ok(Api::Config).await
    }
    async fn stop(&mut self) {
        self.ok(Api::Stop).await;
        let deadline = Instant::now() + Duration::from_secs(15);
        while self
            .process
            .as_mut()
            .unwrap()
            .0
            .try_wait()
            .unwrap()
            .is_none()
        {
            assert!(Instant::now() < deadline, "agent failed graceful stop");
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
        self.process.take();
        cli_ok(&self.root, &["ownership-status"]);
    }
    async fn apply(&self, config: &Value) -> Response {
        self.response(Api::ApplyConfig {
            expected_revision: config["revision"].as_u64().unwrap(),
            settings: config["settings"].clone(),
        })
        .await
    }
}
fn free_tcp() -> std::net::SocketAddr {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
}
fn forward(id: &str, peer: &str, listen: std::net::SocketAddr) -> Value {
    json!({"id":id,"name":id,"peer_node_id":peer,"listen":listen,"target":"127.0.0.1:9","enabled":true,"auto_start":true})
}

fn offline(node: &Node, settings: Value) {
    let path = node.root.join("candidate.json");
    fs::write(&path, serde_json::to_vec(&settings).unwrap()).unwrap();
    cli_ok(
        &node.root,
        &["offline-apply", path.to_str().unwrap(), "--revision", "0"],
    );
}
fn grant(settings: &mut Value, peers: &[String]) {
    settings["trusted_devices"] = json!(peers.iter().map(|peer| json!({"node_id":peer,"display_name":"isolated fixture","last_short_id":null,"trusted_at":1,"updated_at":1})).collect::<Vec<_>>());
    settings["trust_sources"] = json!(
        peers
            .iter()
            .map(|peer| (peer.clone(), json!("administrator")))
            .collect::<serde_json::Map<_, _>>()
    );
    settings["resources"]["peers"] = json!(
        peers
            .iter()
            .map(|peer| json!({"node_id":peer,"files":true,"speed":true,"disabled":false}))
            .collect::<Vec<_>>()
    );
}
async fn wait_peer(node: &Node, peer: &str) {
    let until = Instant::now() + Duration::from_secs(45);
    loop {
        let status = node.ok(Api::Status).await;
        if status["node"]["peers"][peer]["outbound_trusted"] == true {
            return;
        }
        assert!(
            Instant::now() < until,
            "peer not authorized: {status}; {}",
            fs::read_to_string(&node.log).unwrap()
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
async fn echo_request(address: std::net::SocketAddr, data: &[u8]) {
    tokio::time::timeout(Duration::from_secs(25), async {
        let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
        stream.write_all(data).await.unwrap();
        let mut received = vec![0; data.len()];
        stream.read_exact(&mut received).await.unwrap();
        assert_eq!(received, data);
    })
    .await
    .expect("forward request timed out");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn three_agent_processes_unified_files_directory_four_forwards_and_revocation() {
    let sandbox = Sandbox::new();
    let signal_addr = free_tcp();
    let _signal = spawn(
        env!("CARGO_BIN_EXE_p2p_file"),
        &["signal-server", "--listen", &signal_addr.to_string()],
        &sandbox.0.join("signal.log"),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let target = listener.local_addr().unwrap();
    let echo = tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let (mut read, mut write) = stream.split();
                let _ = tokio::io::copy(&mut read, &mut write).await;
            });
        }
    });
    let mut home = Node::init(sandbox.0.join("home"));
    let mut mac = Node::init(sandbox.0.join("mac"));
    let mut win = Node::init(sandbox.0.join("win"));
    let home_id = home.id();
    let mac_id = mac.id();
    let win_id = win.id();
    let mut h = ConfigRecord::load(&home.root.join("node.json"))
        .unwrap()
        .settings
        .redacted();
    h["signal_server"] = json!(signal_addr.to_string());
    grant(&mut h, &[mac_id.clone(), win_id.clone()]);
    h["services"] = json!([{"id":"echo","name":"Echo fixture","target":target,"enabled":true,"allowed_peers":[mac_id,win_id]}]);
    offline(&home, h);
    let ports = [free_tcp(), free_tcp(), free_tcp(), free_tcp()];
    for (node, index) in [(&mac, 0), (&win, 2)] {
        let mut settings = ConfigRecord::load(&node.root.join("node.json"))
            .unwrap()
            .settings
            .redacted();
        settings["signal_server"] = json!(signal_addr.to_string());
        if index == 2 {
            settings["enable_files"] = json!(false);
            settings["enable_speed"] = json!(false);
        }
        grant(&mut settings, std::slice::from_ref(&home_id));
        settings["forwards"] = json!([
            forward("one", &home_id, ports[index]),
            forward("two", &home_id, ports[index + 1])
        ]);
        settings["service_ids"] = json!({"one":"echo","two":"echo"});
        offline(node, settings);
    }
    home.start().await;
    mac.start().await;
    win.start().await;
    for node in [&home, &mac, &win] {
        node.ok(Api::SetPassword {
            expected_revision: 1,
            password: "X7t9Q2k4".into(),
        })
        .await;
    }
    // The restored workbench accepts the same nine-digit device IDs as before.
    let deadline = Instant::now() + Duration::from_secs(15);
    let home_short = loop {
        let status = home.ok(Api::Status).await;
        if let Some(short) = status["node"]["short_id"].as_str() {
            break short.to_owned();
        }
        assert!(
            Instant::now() < deadline,
            "short ID not registered: {status}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    cli_ok(&mac.root, &["connect", &home_short]);
    win.ok(Api::Connect {
        peer: home_id.clone(),
        password: None,
    })
    .await;
    wait_peer(&mac, &home_id).await;
    assert_eq!(
        mac.ok(Api::Status).await["node"]["short_peers"][&home_short],
        home_id
    );
    wait_peer(&win, &home_id).await;
    let services = mac
        .ok(Api::Services {
            peer: home_id.clone(),
        })
        .await;
    assert_eq!(services[0]["id"], "echo");
    for (n, port) in ports.iter().enumerate() {
        echo_request(*port, format!("forward-{n}").as_bytes()).await;
    }
    // Keep a real TCP stream open while the same Agent transfers a file.
    let mut concurrent_tcp = tokio::net::TcpStream::connect(ports[0]).await.unwrap();
    concurrent_tcp.write_all(b"before-file").await.unwrap();
    let mut echoed = [0u8; 11];
    tokio::time::timeout(
        Duration::from_secs(25),
        concurrent_tcp.read_exact(&mut echoed),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(&echoed, b"before-file");
    // Actual IPC send and durable completion on both endpoints.
    let source = mac.root.join("payload.bin");
    let bytes = vec![0x53u8; 180_123];
    fs::write(&source, &bytes).unwrap();
    cli_ok(&mac.root, &["send", &home_id, source.to_str().unwrap()]);
    let until = Instant::now() + Duration::from_secs(30);
    loop {
        let sender = mac.ok(Api::Tasks { offset: 0 }).await;
        let receiver = home.ok(Api::Tasks { offset: 0 }).await;
        if sender["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["state"] == "completed")
            && receiver["tasks"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r["state"] == "completed")
        {
            break;
        }
        assert!(
            Instant::now() < until,
            "file not completed: sender={sender}, receiver={receiver}; {}",
            fs::read_to_string(&home.log).unwrap()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(
        fs::read(home.root.join("receive/payload.bin")).unwrap(),
        bytes
    );
    concurrent_tcp.write_all(b"after-file!").await.unwrap();
    tokio::time::timeout(
        Duration::from_secs(25),
        concurrent_tcp.read_exact(&mut echoed),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(&echoed, b"after-file!");
    drop(concurrent_tcp);
    let directory = mac.root.join("directory-fixture");
    fs::create_dir_all(directory.join("empty")).unwrap();
    fs::write(directory.join("inside.txt"), b"directory payload").unwrap();
    mac.ok(Api::PreviewDirectory {
        peer: home_id.clone(),
        path: directory,
    })
    .await;
    let until = Instant::now() + Duration::from_secs(20);
    let preview = loop {
        let tasks = mac.ok(Api::Tasks { offset: 0 }).await;
        if let Some(preview) = tasks["directory_previews"].as_array().unwrap().first() {
            break preview.clone();
        }
        assert!(
            Instant::now() < until,
            "directory preview not exposed: {tasks}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert!(
        preview.get("source").is_none(),
        "absolute source leaked into preview"
    );
    mac.ok(Api::ConfirmDirectory {
        peer: home_id.clone(),
        request: serde_json::from_value(preview["id"].clone()).unwrap(),
    })
    .await;
    let until = Instant::now() + Duration::from_secs(20);
    while !home
        .root
        .join("receive/directory-fixture/inside.txt")
        .exists()
    {
        assert!(Instant::now() < until, "directory transfer not completed");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(
        fs::read(home.root.join("receive/directory-fixture/inside.txt")).unwrap(),
        b"directory payload"
    );
    assert!(home.root.join("receive/directory-fixture/empty").is_dir());
    // Live receive-directory changes affect new tasks, preserve old files, and
    // fail before persistence when the selected destination is not a directory.
    let previous = home.config().await;
    let mut invalid_root = previous.clone();
    invalid_root["settings"]["receive_root"] = json!(home.root.join("receive/payload.bin"));
    assert!(home.apply(&invalid_root).await.error.is_some());
    assert_eq!(home.config().await, previous);
    let new_root = home.root.join("new-receive");
    fs::create_dir(&new_root).unwrap();
    let mut changed_root = previous.clone();
    changed_root["settings"]["receive_root"] = json!(new_root);
    assert!(home.apply(&changed_root).await.error.is_none());
    let second = mac.root.join("new-location.txt");
    fs::write(&second, b"new destination").unwrap();
    mac.ok(Api::Send {
        peer: home_id.clone(),
        paths: vec![second],
    })
    .await;
    let deadline = Instant::now() + Duration::from_secs(20);
    let received = loop {
        let tasks = home.ok(Api::Tasks { offset: 0 }).await;
        if let Some(task) = tasks["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["name"] == "new-location.txt" && t["state"] == "completed")
        {
            assert_eq!(task["can_delete_file"], true);
            assert_eq!(task["can_pause"], false);
            break task["id"].clone();
        }
        assert!(
            Instant::now() < deadline,
            "new receive root not applied: {tasks}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert_eq!(
        fs::read(new_root.join("new-location.txt")).unwrap(),
        b"new destination"
    );
    assert_eq!(
        fs::read(home.root.join("receive/payload.bin")).unwrap(),
        bytes
    );
    let location = home
        .ok(Api::TaskLocation {
            task: serde_json::from_value(received.clone()).unwrap(),
        })
        .await;
    assert!(Path::new(location["path"].as_str().unwrap()).starts_with(&new_root));
    home.ok(Api::DeleteReceivedFile {
        task: serde_json::from_value(received).unwrap(),
    })
    .await;
    assert!(!new_root.join("new-location.txt").exists());
    let tasks = mac.ok(Api::Tasks { offset: 0 }).await;
    let completed = tasks["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "payload.bin" && t["state"] == "completed")
        .unwrap();
    mac.ok(Api::RemoveHistory {
        tasks: vec![serde_json::from_value(completed["id"].clone()).unwrap()],
    })
    .await;
    assert!(
        source.is_file(),
        "clearing history must preserve the source file"
    );
    let mut limits = mac.config().await;
    limits["settings"]["file_limits"]["upload_kib"] = json!(64);
    assert!(mac.apply(&limits).await.error.is_none());
    let slow = mac.root.join("pause-cancel.bin");
    fs::write(&slow, vec![0x71u8; 8 * 1024 * 1024]).unwrap();
    mac.ok(Api::Send {
        peer: home_id.clone(),
        paths: vec![slow],
    })
    .await;
    let until = Instant::now() + Duration::from_secs(15);
    let task = loop {
        let tasks = mac.ok(Api::Tasks { offset: 0 }).await;
        if let Some(row) = tasks["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["name"] == "pause-cancel.bin" && r["state"] == "transferring")
        {
            break row["id"].clone();
        }
        assert!(Instant::now() < until, "slow file did not start: {tasks}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    let task_id = serde_json::from_value(task.clone()).unwrap();
    mac.ok(Api::Pause { task: task_id }).await;
    let until = Instant::now() + Duration::from_secs(15);
    loop {
        let tasks = mac.ok(Api::Tasks { offset: 0 }).await;
        if tasks["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["id"] == task && r["state"] == "paused")
        {
            break;
        }
        assert!(Instant::now() < until, "pause did not settle: {tasks}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    mac.stop().await;
    mac.start().await;
    let tasks = mac.ok(Api::Tasks { offset: 0 }).await;
    assert!(
        tasks["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["id"] == task && r["state"] == "paused")
    );
    mac.ok(Api::Connect {
        peer: home_id.clone(),
        password: None,
    })
    .await;
    wait_peer(&mac, &home_id).await;
    mac.ok(Api::Resume {
        peer: home_id.clone(),
        task: serde_json::from_value(task.clone()).unwrap(),
    })
    .await;
    mac.ok(Api::Cancel {
        task: serde_json::from_value(task.clone()).unwrap(),
    })
    .await;
    let tasks = mac.ok(Api::Tasks { offset: 0 }).await;
    assert!(
        tasks["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["id"] == task && r["state"] == "failed" && r["retryable"] == false)
    );
    assert!(!home.root.join("receive/pause-cancel.bin").exists());
    mac.ok(Api::StartSpeed {
        peer: home_id.clone(),
        direction: p2p_file::node::config::SpeedtestDirection::Upload,
        seconds: 30,
    })
    .await;
    let until = Instant::now() + Duration::from_secs(15);
    let speed = loop {
        let tasks = mac.ok(Api::Tasks { offset: 0 }).await;
        if let Some(row) = tasks["speeds"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["state"] == "Running")
        {
            break row["id"].clone();
        }
        assert!(
            Instant::now() < until,
            "speed did not start: {tasks}; {}",
            mac.ok(Api::Status).await
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    mac.ok(Api::StopSpeed {
        peer: home_id.clone(),
        task: serde_json::from_value(speed).unwrap(),
    })
    .await;
    // Remove only Mac's service grant; Windows must continue on both listeners.
    let mut config = home.config().await;
    config["settings"]["services"][0]["allowed_peers"] = json!([win_id]);
    assert!(home.apply(&config).await.error.is_none());
    assert_eq!(
        mac.ok(Api::Services {
            peer: home_id.clone()
        })
        .await,
        json!([])
    );
    let mut revoke = home.config().await;
    revoke["settings"]["trusted_devices"]
        .as_array_mut()
        .unwrap()
        .retain(|d| d["node_id"] != mac_id);
    revoke["settings"]["trust_sources"]
        .as_object_mut()
        .unwrap()
        .remove(&mac_id);
    revoke["settings"]["resources"]["peers"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|r| r["node_id"] == mac_id)
        .unwrap()["disabled"] = json!(true);
    assert!(home.apply(&revoke).await.error.is_none());
    echo_request(ports[2], b"win-still-alive-one").await;
    echo_request(ports[3], b"win-still-alive-two").await;
    mac.ok(Api::Disconnect {
        peer: home_id.clone(),
    })
    .await;
    echo_request(ports[2], b"other-peer-disconnect-isolated").await;
    mac.stop().await;
    win.stop().await;
    home.stop().await;
    echo.abort();
    eprintln!(
        "NODE_AGENT_PROOF 3 processes unified authorization, file hash/receipt, directory/empty, 2 peers x 2 forwards, service revoke isolation"
    );
}

#[tokio::test]
async fn agent_process_config_transaction_idempotency_restart_and_ownership() {
    let sandbox = Sandbox::new();
    let mut node = Node::init(sandbox.0.join("node"));
    let identity = node.id();
    node.start().await;
    let duplicate = cli(&node.root, &["init"]);
    assert!(!duplicate.status.success());
    let mut duplicate = spawn(
        env!("CARGO_BIN_EXE_p2p-agent"),
        &["--data-dir", node.root.to_str().unwrap()],
        &node.root.join("duplicate.log"),
    );
    let until = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = duplicate.0.try_wait().unwrap() {
            assert!(!status.success());
            break;
        }
        assert!(Instant::now() < until, "duplicate Agent acquired ownership");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(node.ok(Api::Status).await["node"]["node_id"], identity);
    assert!(!cli(&node.root, &["ownership-status"]).status.success());
    // Bypass client-side validation to exercise the actual host boundary.
    let paths = AgentPaths::new(&node.root).unwrap();
    let mut invalid = Request::new(Api::Status);
    invalid.api_version = 99;
    let mut raw = p2p_file::node::ipc_transport::connect(&paths)
        .await
        .unwrap();
    p2p_file::node::ipc::write(&mut *raw, &invalid)
        .await
        .unwrap();
    let rejected: Response = p2p_file::node::ipc::read(&mut *raw).await.unwrap();
    assert_eq!(rejected.error.unwrap().code, "invalid_request");
    drop(raw);
    let mut raw = p2p_file::node::ipc_transport::connect(&paths)
        .await
        .unwrap();
    raw.write_u32(1024 * 1024 + 1).await.unwrap();
    let mut byte = [0];
    let closed = tokio::time::timeout(Duration::from_secs(6), raw.read(&mut byte))
        .await
        .unwrap();
    assert!(closed.is_err() || closed.unwrap() == 0);
    drop(raw);
    let peer = Identity::generate().node_id().to_hex();
    let a = free_tcp();
    let b = free_tcp();
    let occupied = TcpListener::bind("127.0.0.1:0").unwrap();
    let c = occupied.local_addr().unwrap();
    let mut configuration = node.config().await;
    configuration["settings"]["forwards"] = json!([forward("a", &peer, a), forward("b", &peer, b)]);
    configuration["settings"]["service_ids"] = json!({"a":"echo","b":"echo"});
    assert!(node.apply(&configuration).await.error.is_none());
    let before = fs::read(node.root.join("node.json")).unwrap();
    let committed = node.config().await;
    // Commit failure must preserve the effective root and listeners too.
    let root_path = node.root.join("node.json");
    let held_path = node.root.join("held-node.json");
    fs::rename(&root_path, &held_path).unwrap();
    fs::create_dir(&root_path).unwrap();
    let disk_failure = node.apply(&committed).await;
    fs::remove_dir(&root_path).unwrap();
    fs::rename(&held_path, &root_path).unwrap();
    assert!(disk_failure.error.is_some());
    assert_eq!(node.config().await, committed);
    assert_eq!(fs::read(&root_path).unwrap(), before);
    let mut candidate = committed.clone();
    candidate["settings"]["forwards"] = json!([forward("b", &peer, b), forward("c", &peer, c)]);
    candidate["settings"]["service_ids"] = json!({"b":"echo","c":"echo"});
    assert!(node.apply(&candidate).await.error.is_some());
    assert_eq!(fs::read(node.root.join("node.json")).unwrap(), before);
    assert_eq!(node.config().await, committed);
    assert!(TcpListener::bind(a).is_err(), "failed update removed A");
    assert!(TcpListener::bind(b).is_err(), "failed update removed B");
    let status = node.ok(Api::Status).await;
    assert_eq!(status["node"]["signal"], "unconfigured");
    assert!(status["diagnostics"].as_array().unwrap().is_empty());
    assert!(status["last_error"].is_string());
    assert_eq!(status["forwards"]["rules"].as_array().unwrap().len(), 2);
    drop(occupied);
    let request = Request::new(Api::ApplyConfig {
        expected_revision: committed["revision"].as_u64().unwrap(),
        settings: candidate["settings"].clone(),
    });
    let first = node.client.request(request.clone()).await.unwrap();
    assert!(first.error.is_none(), "{:?}", first.error);
    let retried = node.client.request(request.clone()).await.unwrap();
    assert_eq!(
        first.result.as_ref().unwrap()["revision"],
        retried.result.as_ref().unwrap()["revision"]
    );
    assert_eq!(retried.result.unwrap()["already_committed"], true);
    assert!(TcpListener::bind(a).is_ok());
    assert!(TcpListener::bind(b).is_err());
    assert!(TcpListener::bind(c).is_err());
    assert!(node.ok(Api::Status).await["last_error"].is_null());
    let mut conflict = request.clone();
    conflict.command = Api::StopForward { rule: "b".into() };
    assert_eq!(
        node.client
            .request(conflict)
            .await
            .unwrap()
            .error
            .unwrap()
            .code,
        "id_conflict"
    );
    assert_eq!(
        node.apply(&candidate).await.error.unwrap().code,
        "revision_conflict"
    );
    node.ok(Api::StopForward { rule: "b".into() }).await;
    assert!(TcpListener::bind(b).is_ok());
    let mut update = node.config().await;
    update["settings"]["send_concurrency"] = json!(2);
    assert!(node.apply(&update).await.error.is_none());
    assert!(
        TcpListener::bind(b).is_ok(),
        "ordinary save restarted manually stopped rule"
    );
    node.ok(Api::StartForward { rule: "b".into() }).await;
    assert!(TcpListener::bind(b).is_err());
    let mut restart = node.config().await;
    restart["settings"]["enable_speed"] = json!(false);
    assert_eq!(
        node.apply(&restart).await.error.unwrap().code,
        "restart_required"
    );
    let events = node
        .ok(Api::Events {
            epoch: String::new(),
            after: 0,
        })
        .await;
    assert_eq!(events["resync_required"], true);
    assert_eq!(events["revision"], restart["revision"]);
    node.stop().await;
    assert!(TcpListener::bind(b).is_ok());
    assert!(TcpListener::bind(c).is_ok());
    node.start().await;
    assert_eq!(node.ok(Api::Status).await["node"]["node_id"], identity);
    // Last persisted ID survives process restart, even though the in-memory cache doesn't.
    let saved = ConfigRecord::load(&node.root.join("node.json")).unwrap();
    assert_eq!(saved.revision, restart["revision"].as_u64().unwrap());
    let current = node.config().await;
    let mut settings = current["settings"].clone();
    settings["enable_speed"] = json!(false);
    node.ok(Api::ConfigureRestart {
        expected_revision: current["revision"].as_u64().unwrap(),
        settings,
    })
    .await;
    let deadline = Instant::now() + Duration::from_secs(15);
    while node
        .process
        .as_mut()
        .unwrap()
        .0
        .try_wait()
        .unwrap()
        .is_none()
    {
        assert!(
            Instant::now() < deadline,
            "ConfigureRestart did not exit host"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    node.process.take();
    cli_ok(&node.root, &["ownership-status"]);
    node.start().await;
    assert_eq!(node.config().await["settings"]["enable_speed"], false);
    node.stop().await;
    eprintln!(
        "NODE_AGENT_PROOF config rollback A/B, occupied C, revision/idempotency, manual stop, restart, locks"
    );
}
