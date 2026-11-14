use super::*;
use std::{
    future::Future,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

const LOCAL: &str = "11111111111111111111111111111111";
const REMOTE: &str = "22222222222222222222222222222222";
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct TestDir {
    path: PathBuf,
}

impl TestDir {
    fn new() -> Self {
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "vey-navigation-manager-test-{}-{sequence}-{nonce}",
            std::process::id(),
        ));
        std::fs::create_dir(&path).unwrap();
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        if self
            .path
            .file_name()
            .and_then(|value| value.to_str())
            .is_some_and(|value| value.starts_with("vey-navigation-manager-test-"))
            && self.path.parent() == Some(std::env::temp_dir().as_path())
        {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}

#[derive(Clone)]
struct FakeBackend {
    state: Arc<Mutex<FakeState>>,
}

struct FakeState {
    local: Value,
    remote: Value,
    commands: Vec<(String, bool, Option<String>)>,
    target_probe: Value,
    restart_calls: usize,
    fail_local_uncertain: bool,
}

impl FakeBackend {
    fn new(local: Value, remote: Value) -> Self {
        Self {
            state: Arc::new(Mutex::new(FakeState {
                local,
                remote,
                commands: Vec::new(),
                target_probe: json!({"reachable":true,"target":"127.0.0.1:8080"}),
                restart_calls: 0,
                fail_local_uncertain: false,
            })),
        }
    }

    fn local(&self) -> Value {
        self.state.lock().unwrap().local.clone()
    }

    fn remote(&self) -> Value {
        self.state.lock().unwrap().remote.clone()
    }

    fn set_local_uncertain(&self, value: bool) {
        self.state.lock().unwrap().fail_local_uncertain = value;
    }

    fn commands(&self) -> Vec<(String, bool, Option<String>)> {
        self.state.lock().unwrap().commands.clone()
    }
}

fn ready<'a>(future: impl Future<Output = Result<Value>> + Send + 'a) -> super::super::Call<'a> {
    Box::pin(future)
}

impl Backend for FakeBackend {
    fn call<'a>(
        &'a self,
        command: Value,
        remote: bool,
        request_id: Option<&'a str>,
    ) -> super::super::Call<'a> {
        ready(async move {
            let method = command
                .get("method")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            let mut state = self.state.lock().unwrap();
            state
                .commands
                .push((method.clone(), remote, request_id.map(str::to_owned)));
            match method.as_str() {
                "config" => Ok(if remote {
                    state.remote.clone()
                } else {
                    state.local.clone()
                }),
                "status" => Ok(if remote {
                    json!({"node":{"node_id":REMOTE},"core_running":true,"epoch":"remote"})
                } else {
                    json!({
                        "node":{
                            "node_id":LOCAL,
                            "peers":{REMOTE:{"phase":"connected","transport":{"rtt_us":1250}}},
                            "signal":{},
                        },
                        "core_running":true,
                        "epoch":"local",
                        "forwards":{"active_streams":3,"rules":[{
                            "id":"admin","listening":true,"peer_ready":true,"last_error":null
                        }]},
                        "diagnostics":[],
                    })
                }),
                "services" => Ok(json!([{"id":"ubuntu-admin","name":"Remote admin"}])),
                "apply_config" => {
                    if !remote && state.fail_local_uncertain {
                        return Err(Problem {
                            message: "local apply outcome unknown".into(),
                            code: "outcome_unknown".into(),
                            uncertain: true,
                            details: None,
                        });
                    }
                    let target = if remote {
                        &mut state.remote
                    } else {
                        &mut state.local
                    };
                    let expected = command
                        .get("expected_revision")
                        .and_then(Value::as_u64)
                        .unwrap_or(u64::MAX);
                    if target.get("revision").and_then(Value::as_u64) != Some(expected) {
                        return Err(Problem::code("revision conflict", "revision_conflict"));
                    }
                    target["settings"] = command.get("settings").cloned().unwrap_or(Value::Null);
                    let revision = expected + 1;
                    target["revision"] = json!(revision);
                    Ok(json!({"revision":revision}))
                }
                "start_forward" => Ok(json!({"listening":true})),
                "stop_forward" => Ok(json!({"stopped":true})),
                "disconnect" => Ok(json!({"accepted":true})),
                "connect" => Ok(json!({"accepted":true})),
                _ => Err(Problem::new(format!("unexpected command {method}"))),
            }
        })
    }

    fn target_probe<'a>(&'a self, _service: &'a str) -> super::super::Call<'a> {
        ready(async move { Ok(self.state.lock().unwrap().target_probe.clone()) })
    }

    fn restart<'a>(&'a self) -> super::super::Call<'a> {
        ready(async move {
            self.state.lock().unwrap().restart_calls += 1;
            Ok(json!({"message":"restarted"}))
        })
    }
}

struct Fixture {
    _temp: TestDir,
    backend: FakeBackend,
    manager: Manager,
    config: Value,
    host: Value,
}

impl Fixture {
    fn new() -> Self {
        Self::with_receipts(None)
    }

    fn with_receipts(receipts: Option<Value>) -> Self {
        let temp = TestDir::new();
        let navigation = temp.path().join("navigation");
        std::fs::create_dir_all(navigation.join("control")).unwrap();
        if let Some(receipts) = receipts {
            std::fs::write(
                navigation.join("control/operations.json"),
                serde_json::to_vec_pretty(&receipts).unwrap(),
            )
            .unwrap();
        }
        let page = navigation.join("service-navigation.html");
        let baseline = json!({
            "updatedAt":"2026-11-14T00:00:00Z",
            "host":{"platform":"linux","name":"Primary"},
            "services":[{
                "id":"admin",
                "serviceId":"ubuntu-admin",
                "peerNodeId":REMOTE,
                "name":"Remote admin",
                "title":"Remote admin",
                "description":"Open admin",
                "listen":"127.0.0.1:28081",
                "target":"127.0.0.1:8080",
                "remote":true,
                "protocol":"http",
                "category":"节点管理",
                "url":"http://127.0.0.1:28081/",
                "remoteLabel":"远端 Ubuntu",
                "enabled":true,
                "ruleEnabled":true,
                "autoStart":true,
                "healthPath":""
            }]
        });
        std::fs::write(
            &page,
            format!(
                "<!doctype html>{DATA_OPEN}\n{}\n{DATA_CLOSE}",
                serde_json::to_string(&baseline).unwrap()
            ),
        )
        .unwrap();
        let settings = json!({
            "forwards":[{
                "id":"admin",
                "name":"Remote admin",
                "peer_node_id":REMOTE,
                "listen":"127.0.0.1:28081",
                "target":"127.0.0.1:1",
                "enabled":true,
                "auto_start":true
            }],
            "service_ids":{"admin":"ubuntu-admin"},
            "trusted_devices":[{"node_id":REMOTE,"name":"Ubuntu"}],
            "signal_server":"preserved",
            "resources":{"preserved":true}
        });
        let remote_settings = json!({
            "services":[{
                "id":"ubuntu-admin",
                "name":"Ubuntu admin",
                "target":"127.0.0.1:8080",
                "enabled":true,
                "allowed_peers":[LOCAL]
            }],
            "other":"preserved"
        });
        let local = json!({"revision":1,"settings":settings});
        let remote = json!({"revision":7,"settings":remote_settings});
        let backend = FakeBackend::new(local, remote);
        let config = json!({
            "local_name":"Primary",
            "page":page,
            "state_dir":navigation.join("control"),
            "remote":{
                "name":"Ubuntu",
                "node_id":REMOTE,
                "management_service_id":"ubuntu-admin"
            },
            "peers":{"22222222222222222222222222222222":{"name":"Ubuntu","platform":"linux"}}
        });
        let host = json!({
            "platform":"linux",
            "name":"Primary",
            "localAdminId":"local-admin",
            "adminUrl":"http://127.0.0.1:7788/"
        });
        let manager =
            Manager::new(config.clone(), host.clone(), Arc::new(backend.clone())).unwrap();
        Self {
            _temp: temp,
            backend,
            manager,
            config,
            host,
        }
    }

    fn form(&self) -> Value {
        json!({
            "id":"admin",
            "name":"Remote admin edited",
            "peer":REMOTE,
            "serviceId":"ubuntu-admin",
            "listen":"127.0.0.1:28081",
            "target":"127.0.0.1:8080",
            "enabled":true,
            "autoStart":true,
            "protocol":"http",
            "category":"节点管理",
            "path":"/",
            "healthPath":""
        })
    }
}

#[tokio::test]
async fn state_and_navigation_keep_agent_forwarding_in_place() {
    let mut fixture = Fixture::new();
    let state = fixture.manager.state(false).await.unwrap();
    assert_eq!(state["host"]["name"], "Primary");
    assert_eq!(state["navigation"]["services"][0]["id"], "admin");
    assert_eq!(state["nodes"][0]["rttMs"], 1.2);
    assert_eq!(state["rules"][0]["listening"], true);
    assert!(
        fixture
            .backend
            .commands()
            .iter()
            .all(|(method, _, _)| method != "stop" && method != "apply_config")
    );
}

#[tokio::test]
async fn preview_and_apply_preserve_unrelated_configuration_without_restart() {
    let mut fixture = Fixture::new();
    let preview = fixture.manager.preview(&fixture.form()).await.unwrap();
    let plan = preview["plan"].as_str().unwrap();
    let result = fixture
        .manager
        .mutate("apply", &json!({"plan":plan,"operation":"a".repeat(32)}))
        .await
        .unwrap();
    assert_eq!(result["ruleId"], "admin");
    let local = fixture.backend.local();
    assert_eq!(local["revision"], 2);
    assert_eq!(
        local["settings"]["forwards"][0]["name"],
        "Remote admin edited"
    );
    assert_eq!(local["settings"]["signal_server"], "preserved");
    assert_eq!(local["settings"]["resources"]["preserved"], true);
    assert!(
        fixture
            .backend
            .commands()
            .iter()
            .all(|(method, _, _)| method != "stop")
    );
    let html = std::fs::read_to_string(fixture.config["page"].as_str().unwrap()).unwrap();
    assert!(html.contains("Remote admin edited"));
    assert_eq!(html.matches(DATA_OPEN).count(), 1);
    let operations: Value = serde_json::from_slice(
        &std::fs::read(
            Path::new(fixture.config["state_dir"].as_str().unwrap()).join("operations.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        operations["aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"]["state"],
        "complete"
    );
    let presentation: Value = serde_json::from_slice(
        &std::fs::read(
            Path::new(fixture.config["state_dir"].as_str().unwrap()).join("presentation.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(presentation["admin"]["serviceId"], "ubuntu-admin");
}

#[tokio::test]
async fn completed_receipt_is_replayed_after_manager_reload() {
    let mut fixture = Fixture::new();
    let preview = fixture.manager.preview(&fixture.form()).await.unwrap();
    let body = json!({"plan":preview["plan"],"operation":"b".repeat(32)});
    let first = fixture.manager.mutate("apply", &body).await.unwrap();
    let apply_count = fixture
        .backend
        .commands()
        .iter()
        .filter(|(method, _, _)| method == "apply_config")
        .count();
    let mut reloaded = Manager::new(
        fixture.config.clone(),
        fixture.host.clone(),
        Arc::new(fixture.backend.clone()),
    )
    .unwrap();
    let second = reloaded.mutate("apply", &body).await.unwrap();
    assert_eq!(first, second);
    let second_count = fixture
        .backend
        .commands()
        .iter()
        .filter(|(method, _, _)| method == "apply_config")
        .count();
    assert_eq!(second_count, apply_count);
}

#[tokio::test]
async fn reusing_an_operation_for_different_payload_is_a_conflict() {
    let mut fixture = Fixture::new();
    let preview = fixture.manager.preview(&fixture.form()).await.unwrap();
    let operation = "e".repeat(32);
    fixture
        .manager
        .mutate(
            "apply",
            &json!({"plan":preview["plan"],"operation":operation}),
        )
        .await
        .unwrap();
    let error = fixture
        .manager
        .mutate("stop", &json!({"rule":"admin","operation":operation}))
        .await
        .unwrap_err();
    assert_eq!(error.code, "id_conflict");
}

#[tokio::test]
async fn legacy_python_complete_and_pending_receipts_replay_without_commands() {
    const OPERATION: &str = "0123456789abcdef0123456789abcdef";
    const PYTHON_DIGEST: &str = "23662a00c7a2aa111b7afed2917e472d3438a43404d8af30dbe68188bfe03902";
    let body = json!({"operation":OPERATION,"plan":"legacy-plan"});
    let receipt = |state: &str, result: Value, error: Value| {
        let mut value = json!({
            "action":"apply",
            "at":"2026-11-14T10:00:00+08:00",
            "digest":PYTHON_DIGEST,
            "state":state,
            "subject":"legacy subject",
        });
        if !result.is_null() {
            value["result"] = result;
        }
        if !error.is_null() {
            value["error"] = error;
        }
        value
    };
    let mut complete_receipts = Map::new();
    complete_receipts.insert(
        OPERATION.into(),
        receipt(
            "complete",
            json!({"message":"legacy complete","revision":9}),
            Value::Null,
        ),
    );
    let mut complete = Fixture::with_receipts(Some(Value::Object(complete_receipts)));
    let replayed = complete.manager.mutate("apply", &body).await.unwrap();
    assert_eq!(replayed["message"], "legacy complete");
    assert!(complete.backend.commands().is_empty());

    let mut pending_receipts = Map::new();
    pending_receipts.insert(
        OPERATION.into(),
        receipt("pending", Value::Null, Value::Null),
    );
    let mut pending = Fixture::with_receipts(Some(Value::Object(pending_receipts)));
    let error = pending.manager.mutate("apply", &body).await.unwrap_err();
    assert_eq!(error.code, "outcome_unknown");
    assert!(error.uncertain);
    assert!(pending.backend.commands().is_empty());
}

#[tokio::test]
async fn history_orders_python_offsets_and_new_utc_stamps_chronologically() {
    let fixture = Fixture::with_receipts(Some(json!({
        "legacy":{
            "action":"start",
            "at":"2026-11-14T10:00:00+08:00",
            "digest":"ignored",
            "state":"complete",
            "subject":"legacy",
            "result":{"message":"legacy event"}
        },
        "current":{
            "action":"stop",
            "at":"2026-11-14T03:00:00Z",
            "digest":"ignored",
            "state":"complete",
            "subject":"current",
            "result":{"message":"current event"}
        }
    })));
    let history = fixture.manager.history();
    assert_eq!(history[0]["id"], "current");
    assert_eq!(history[1]["id"], "legacy");
    assert_eq!(
        epoch_seconds_from_iso_stamp("2026-11-14T10:00:00+08:00"),
        epoch_seconds_from_iso_stamp("2026-11-14T02:00:00Z")
    );
}

#[tokio::test]
async fn revision_conflict_happens_before_any_agent_write() {
    let mut fixture = Fixture::new();
    let preview = fixture.manager.preview(&fixture.form()).await.unwrap();
    {
        let mut backend = fixture.backend.state.lock().unwrap();
        backend.local["revision"] = json!(2);
    }
    let error = fixture
        .manager
        .mutate(
            "apply",
            &json!({"plan":preview["plan"],"operation":"c".repeat(32)}),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, "revision_conflict");
    assert!(
        fixture
            .backend
            .commands()
            .iter()
            .all(|(method, _, _)| method != "apply_config")
    );
}

#[tokio::test]
async fn remote_target_change_warns_and_local_unknown_does_not_rollback() {
    let mut fixture = Fixture::new();
    {
        let mut backend = fixture.backend.state.lock().unwrap();
        backend.local["settings"]["forwards"]
            .as_array_mut()
            .unwrap()
            .push(json!({
                "id":"web",
                "name":"Web",
                "peer_node_id":REMOTE,
                "listen":"127.0.0.1:28082",
                "target":"127.0.0.1:1",
                "enabled":true,
                "auto_start":true
            }));
        backend.local["settings"]["service_ids"]["web"] = json!("web");
        backend.remote["settings"]["services"]
            .as_array_mut()
            .unwrap()
            .push(json!({
                "id":"web",
                "name":"Web",
                "target":"127.0.0.1:8090",
                "enabled":true,
                "allowed_peers":[LOCAL,REMOTE]
            }));
    }
    let mut form = fixture.form();
    form["id"] = json!("web");
    form["name"] = json!("Web edited");
    form["serviceId"] = json!("web");
    form["listen"] = json!("127.0.0.1:28082");
    form["target"] = json!("127.0.0.1:8091");
    let preview = fixture.manager.preview(&form).await.unwrap();
    assert!(
        preview["warnings"][0]
            .as_str()
            .unwrap()
            .contains("共享服务目标")
    );
    fixture.backend.set_local_uncertain(true);
    let error = fixture
        .manager
        .mutate(
            "apply",
            &json!({"plan":preview["plan"],"operation":"d".repeat(32)}),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, "partial_unknown");
    assert!(error.uncertain);
    assert_eq!(
        fixture.backend.remote()["settings"]["services"][1]["target"],
        "127.0.0.1:8091"
    );
    let applies = fixture
        .backend
        .commands()
        .into_iter()
        .filter(|(method, _, _)| method == "apply_config")
        .collect::<Vec<_>>();
    assert!(applies.iter().any(|(_, remote, _)| *remote));
    assert_eq!(applies.iter().filter(|(_, remote, _)| !*remote).count(), 1);
    let repeated = fixture
        .manager
        .mutate(
            "apply",
            &json!({"plan":preview["plan"],"operation":"d".repeat(32)}),
        )
        .await
        .unwrap_err();
    assert_eq!(repeated.code, "partial_unknown");
    assert_eq!(
        fixture
            .backend
            .commands()
            .iter()
            .filter(|(method, _, _)| method == "apply_config")
            .count(),
        applies.len()
    );
}

#[tokio::test]
async fn protected_management_service_identity_cannot_be_reassigned() {
    let mut fixture = Fixture::new();
    let mut form = fixture.form();
    form["serviceId"] = json!("different-service");
    let error = fixture.manager.preview(&form).await.unwrap_err();
    assert!(error.message.contains("请保留远端管理服务"));
}
