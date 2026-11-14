use super::*;
use std::fs;

#[test]
fn successful_real_agent_envelopes_accept_null_error_fields() {
    let response = ipc::Response::ok("remote-fixture", json!({"revision": 26}));
    let output = ProcessOutput {
        success: true,
        stdout: serde_json::to_vec(&response).unwrap(),
        stderr: Vec::new(),
    };
    assert_eq!(remote_result(&output).unwrap()["revision"], 26);
    let output = ProcessOutput {
        success: false,
        stdout: serde_json::to_vec(&ipc::Response::error(
            "remote-fixture",
            "revision_conflict",
            "changed remotely",
        ))
        .unwrap(),
        stderr: Vec::new(),
    };
    assert_eq!(
        remote_result(&output).unwrap_err().code,
        "revision_conflict"
    );
}

fn scratch(tag: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "veytrane-navigation-platform-{tag}-{:032x}",
        rand::random::<u128>()
    ));
    fs::create_dir_all(&path).unwrap();
    path
}

fn current_config(root: &Path) -> Value {
    let navigation_root = root.join("private/navigation");
    let mut config = default_config_for(current_platform().unwrap(), &navigation_root).unwrap();
    config["installed"] = json!(path_text(&root.join("installed.json")).unwrap());
    config["lifecycle"]["kind"] = json!("none");
    config["lifecycle"]["service"] = Value::Null;
    config["lifecycle"]["definition"] = Value::Null;
    config["config_file"] = json!(path_text(&navigation_root.join("config.json")).unwrap());
    config
}

#[test]
fn missing_config_derives_navigation_paths_without_writing_them() {
    let root = scratch("missing");
    let navigation = root.join("custom-navigation");
    let path = navigation.join("config.json");
    let config = load_config(&path).unwrap();
    assert_eq!(config["http"]["port"], 3001);
    assert_eq!(config["http"]["public_port"], 3001);
    assert_eq!(
        config["page"],
        json!(path_text(&navigation.join("service-navigation.html")).unwrap())
    );
    assert_eq!(
        config["state_dir"],
        json!(path_text(&navigation.join("control")).unwrap())
    );
    assert_eq!(config["config_file"], json!(path_text(&path).unwrap()));
    assert!(!navigation.exists());
    let _ = fs::remove_dir_all(root);
}

#[test]
fn relative_legacy_paths_resolve_from_the_config_directory() {
    let root = scratch("relative");
    let navigation = root.join("navigation");
    fs::create_dir_all(&navigation).unwrap();
    let path = navigation.join("config.json");
    fs::write(
        &path,
        serde_json::to_vec_pretty(&json!({
            "local_name": "Workstation",
            "installed": "agent/installed.json",
            "page": "public/service-navigation.html",
            "state_dir": "private/control",
            "http": {"port": 18880},
            "lifecycle": {"definition": "agent.service"}
        }))
        .unwrap(),
    )
    .unwrap();
    let config = load_config(&path).unwrap();
    assert_eq!(
        config["installed"],
        json!(path_text(&navigation.join("agent/installed.json")).unwrap())
    );
    assert_eq!(
        config["page"],
        json!(path_text(&navigation.join("public/service-navigation.html")).unwrap())
    );
    assert_eq!(config["http"]["port"], 18880);
    assert_eq!(config["http"]["public_port"], 3001);
    assert_eq!(
        config["lifecycle"]["definition"],
        json!(path_text(&navigation.join("agent.service")).unwrap())
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn legacy_mac_loopback_gateway_migrates_to_the_shared_port() {
    let root = scratch("legacy-port");
    let navigation = root.join("navigation");
    let path = navigation.join("config.json");
    let defaults = default_config_for("macos", &navigation).unwrap();
    let config = normalize_config(
        defaults,
        Some(&json!({
            "http": {"port": 17790, "public_port": 3001}
        })),
        &path,
    )
    .unwrap();
    assert_eq!(config["http"]["port"], 3001);
    assert_eq!(config["http"]["public_port"], 3001);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn host_info_exposes_builtin_restart_and_keeps_config_identity() {
    let root = scratch("host-info");
    let config = current_config(&root);
    validate_config(&config).unwrap();
    let host = host_info(&config).unwrap();
    assert_eq!(host["name"], config["local_name"]);
    assert_eq!(host["adminUrl"], config["agent_web"]);
    assert_eq!(host["agentRestartSupported"], true);
    assert_eq!(host["ui"]["restartSupported"], true);
    assert_eq!(
        host["localAdminId"],
        if current_platform().unwrap() == "macos" {
            "mac-admin"
        } else {
            "local-admin"
        }
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn config_validation_rejects_non_loopback_admin_and_invalid_peer_targets() {
    let root = scratch("validation");
    let mut config = current_config(&root);
    config["agent_web"] = json!("http://example.com/control");
    assert!(validate_config(&config).is_err());

    let mut config = current_config(&root);
    config["peers"] = json!({
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa": {
            "name": "Office",
            "platform": "windows",
            "targets": {"rdp": ""}
        }
    });
    assert!(validate_config(&config).is_err());
    let _ = fs::remove_dir_all(root);
}

#[test]
fn installed_metadata_is_optional_but_never_allowed_to_select_another_agent() {
    let root = scratch("installed");
    let data = root.join("data");
    fs::create_dir_all(&data).unwrap();
    let paths = AgentPaths::new(&data).unwrap();
    let config = current_config(&root);
    let installed = root.join("installed.json");
    let (_restart, _receiver) = watch::channel(false);

    Bridge::new(config.clone(), paths.clone(), _restart.clone()).unwrap();

    fs::write(
        &installed,
        serde_json::to_vec(&json!({
            "bin_dir": root.join("bin"),
            "data_dir": root.join("other-data")
        }))
        .unwrap(),
    )
    .unwrap();
    let error = Bridge::new(config.clone(), paths.clone(), _restart.clone())
        .err()
        .unwrap();
    assert_eq!(error.code, "agent_host_mismatch");

    fs::write(
        &installed,
        serde_json::to_vec(&json!({
            "bin_dir": root.join("bin"),
            "data_dir": data
        }))
        .unwrap(),
    )
    .unwrap();
    Bridge::new(config, paths, _restart).unwrap();
    let _ = fs::remove_dir_all(root);
}

#[tokio::test(start_paused = true)]
async fn restart_is_accepted_before_a_toggled_watch_notification() {
    let root = scratch("restart");
    let data = root.join("data");
    fs::create_dir_all(&data).unwrap();
    let paths = AgentPaths::new(&data).unwrap();
    let config = current_config(&root);
    let (restart, mut restarted) = watch::channel(false);
    let bridge = Bridge::new(config, paths, restart).unwrap();

    let first = bridge.restart().await.unwrap();
    assert_eq!(first["accepted"], true);
    assert!(first["message"].as_str().unwrap().contains("请稍后刷新"));
    tokio::task::yield_now().await;
    assert!(!restarted.has_changed().unwrap());
    tokio::time::advance(Duration::from_millis(999)).await;
    tokio::task::yield_now().await;
    assert!(!restarted.has_changed().unwrap());
    tokio::time::advance(Duration::from_millis(1)).await;
    tokio::task::yield_now().await;
    assert!(restarted.has_changed().unwrap());
    assert!(*restarted.borrow_and_update());

    bridge.restart().await.unwrap();
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(1)).await;
    tokio::task::yield_now().await;
    assert!(restarted.has_changed().unwrap());
    assert!(!*restarted.borrow_and_update());
    let _ = fs::remove_dir_all(root);
}

#[test]
fn remote_bootstrap_runs_the_installed_cli_instead_of_a_python_backend() {
    let root = scratch("remote");
    let navigation = root.join("navigation");
    let path = navigation.join("config.json");
    let config = normalize_config(
        default_config_for(current_platform().unwrap(), &navigation).unwrap(),
        Some(&json!({
            "remote": {
                "name": "Remote Linux",
                "node_id": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "ssh_host": "remote-agent",
                "python": "python3",
                "installed": "~/custom bin/installed.json"
            }
        })),
        &path,
    )
    .unwrap();
    let request = remote_script(&config, RemoteOperation::Request).unwrap();
    let probe = remote_script(&config, RemoteOperation::Probe).unwrap();
    assert!(request.contains("\"p2p-node\""));
    assert!(request.contains("\"request\",\"-\""));
    assert!(probe.contains("Path(i[\"data_dir\"])/\"node.json\""));
    assert!(probe.contains("try:\n c="));
    assert!(probe.contains("except OSError:\n print("));
    assert!(!request.contains("http.server"));
    assert!(!request.contains("navigation_server"));

    let argv = ssh_argv(&config, &probe).unwrap();
    assert_eq!(argv[0], "ssh");
    assert_eq!(argv[5], "remote-agent");
    assert!(argv[6].starts_with("python3 -c "));
    assert!(!argv[6].contains('\n'));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn base64_bootstrap_matches_standard_padding() {
    assert_eq!(base64_encode(b""), "");
    assert_eq!(base64_encode(b"f"), "Zg==");
    assert_eq!(base64_encode(b"fo"), "Zm8=");
    assert_eq!(base64_encode(b"foo"), "Zm9v");
    assert_eq!(base64_encode(b"foobar"), format!("{}{}", "Zm9v", "YmFy"));
}
