//! Single local owner for identity, network, files and management state.
use super::{
    agent_config::ConfigRecord,
    agent_paths::AgentPaths,
    control::{self, NodeControl, SequencedEvent},
    ipc::{self, Command, Request, Response},
};
use crate::identity::{Identity, NodeId};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, VecDeque},
    io,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{
    sync::{Mutex, Semaphore, watch},
    task::JoinSet,
};

struct Cached {
    digest: String,
    created: Instant,
    response: watch::Sender<Option<Response>>,
}
struct Journal {
    events: VecDeque<SequencedEvent>,
    floor: u64,
}
pub struct Agent {
    pub(crate) control: NodeControl,
    pub(crate) config: Mutex<ConfigRecord>,
    pub(crate) paths: AgentPaths,
    epoch: String,
    journal: Mutex<Journal>,
    cache: Mutex<HashMap<String, Cached>>,
    pub(crate) mutations: Mutex<()>,
    workers: Mutex<JoinSet<()>>,
    pub(crate) last_error: Mutex<Option<String>>,
    pub(crate) forwards: Mutex<super::agent_forward::ForwardManager>,
}
impl Agent {
    async fn execute(&self, command: Command, id: &str, digest: &str) -> Result<Value, String> {
        match command {
            Command::Status => Ok(
                json!({"data_dir":self.paths.root,"epoch":self.epoch,"revision":self.config.lock().await.revision,"node":self.control.snapshot().map_err(|e|e.to_string())?,"last_error":*self.last_error.lock().await,"forwards":self.forwards.lock().await.snapshot()}),
            ),
            Command::Config => {
                let record = self.config.lock().await;
                Ok(
                    json!({"revision":record.revision,"settings":record.settings.redacted(),"password_configured":record.settings.remote_auth.is_some()}),
                )
            }
            Command::Tasks => {
                let snapshot = self.control.task_snapshot().map_err(|e| e.to_string())?;
                let tasks=snapshot.as_ref().map(|s| s.tasks.iter().take(128).map(|t|json!({"id":t.id,"peer":t.peer.to_hex(),"name":t.name,"state":t.state,"direction":t.direction,"total":t.total,"confirmed":t.confirmed,"rate":t.rate,"retryable":t.retryable})).collect::<Vec<_>>()).unwrap_or_default();
                Ok(json!({"tasks":tasks,"truncated":snapshot.is_some_and(|s|s.tasks.len()>128)}))
            }
            Command::Events { epoch, after } => {
                let journal = self.journal.lock().await;
                let snapshot = self.control.snapshot().map_err(|e| e.to_string())?;
                let lost = epoch != self.epoch
                    || after < journal.floor
                    || after > snapshot.sequence
                    || journal
                        .events
                        .front()
                        .is_some_and(|first| after.saturating_add(1) < first.sequence);
                let events = if lost {
                    vec![]
                } else {
                    journal
                        .events
                        .iter()
                        .filter(|e| e.sequence > after)
                        .take(128)
                        .cloned()
                        .collect()
                };
                Ok(
                    json!({"epoch":self.epoch,"resync_required":lost,"snapshot":if lost {Some(snapshot)} else {None},"events":events}),
                )
            }
            Command::Services { peer } => Ok(json!(
                self.control
                    .list_services(NodeId::from_hex(&peer).map_err(|e| e.to_string())?)
                    .await?
            )),
            Command::Connect { peer, password } => {
                let peer = NodeId::from_hex(&peer).map_err(|e| e.to_string())?;
                if let Some(password) = password {
                    self.control.connect_peer_with_password(
                        peer,
                        super::remote_auth::SecretPassword::new(password)
                            .map_err(|e| e.to_string())?,
                    )?;
                } else {
                    self.control.connect_peer_trusted(peer)?;
                }
                Ok(json!({"accepted":true}))
            }
            Command::Disconnect { peer } => {
                self.control
                    .disconnect_peer(NodeId::from_hex(&peer).map_err(|e| e.to_string())?)
                    .await?;
                Ok(json!({"disconnected":true}))
            }
            Command::Send { peer, paths } => {
                if paths.is_empty() || paths.len() > 128 || paths.iter().any(|p| !p.is_absolute()) {
                    return Err("发送路径必须是1..128个本机绝对路径".into());
                }
                self.control
                    .send_paths(NodeId::from_hex(&peer).map_err(|e| e.to_string())?, paths)?;
                Ok(json!({"accepted":true}))
            }
            Command::Pause { task } => {
                self.control.pause_task(task)?;
                Ok(json!({"accepted":true}))
            }
            Command::Resume { peer, task } => {
                self.control
                    .resume_task(NodeId::from_hex(&peer).map_err(|e| e.to_string())?, task)?;
                Ok(json!({"accepted":true}))
            }
            Command::Cancel { task } => {
                self.control
                    .cancel_task(task)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(json!({"cancelled":true}))
            }
            Command::StartForward { rule } => {
                let record = self.config.lock().await;
                if !record
                    .settings
                    .forwards
                    .iter()
                    .any(|r| r.id == rule && r.enabled)
                {
                    return Err("转发规则不存在或已禁用".into());
                }
                let mut forwards = self.forwards.lock().await;
                let plan = forwards
                    .prepare(&record.settings, &self.control, Some(&rule))
                    .await
                    .map_err(|e| e.to_string())?;
                let retired = forwards.commit(plan);
                drop(forwards);
                for task in retired {
                    let _ = task.await;
                }
                Ok(json!({"listening":true}))
            }
            Command::StopForward { rule } => {
                self.forwards
                    .lock()
                    .await
                    .stop(&rule)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(json!({"stopped":true}))
            }
            Command::ApplyConfig {
                expected_revision,
                settings,
            } => {
                self.apply_settings(expected_revision, settings, id, digest)
                    .await
            }
            Command::SetPassword {
                expected_revision,
                password,
            } => {
                let verifier = tokio::task::spawn_blocking(move || {
                    super::remote_auth::SecretPassword::new(password)
                        .and_then(|p| super::remote_auth::RemoteVerifier::create(&p))
                })
                .await
                .map_err(|e| e.to_string())?
                .map_err(|e| e.to_string())?;
                let mut record = self.config.lock().await;
                let mut settings = record.settings.clone();
                settings.remote_auth = Some(verifier);
                self.commit_settings(&mut record, expected_revision, settings, id, digest)
                    .await
            }
            Command::Stop => Ok(json!({"stopping":true})),
        }
    }

    async fn submit(self: &Arc<Self>, request: Request) -> Response {
        if let Err(error) = request.validate() {
            return Response::error(&request.request_id, "invalid_request", error);
        }
        if request.command.is_query() {
            return match tokio::time::timeout(
                Duration::from_millis(request.timeout_ms.into()),
                self.execute(request.command, &request.request_id, ""),
            )
            .await
            {
                Ok(Ok(value)) => Response::ok(&request.request_id, value),
                Ok(Err(error)) => Response::error(&request.request_id, "command_failed", error),
                Err(_) => Response::error(&request.request_id, "timeout", "查询超时"),
            };
        }
        let bytes =
            serde_json::to_vec(&serde_json::to_value(&request.command).expect("command JSON"))
                .expect("command bytes");
        let digest = blake3::hash(&bytes).to_hex().to_string();
        {
            let record = self.config.lock().await;
            if let Some((last, hash)) = &record.last_request
                && last == &request.request_id
            {
                return if hash == &digest {
                    Response::ok(
                        last,
                        json!({"revision":record.revision,"already_committed":true}),
                    )
                } else {
                    Response::error(last, "id_conflict", "已持久提交的请求ID包含不同命令")
                };
            }
        }
        let mut cache = self.cache.lock().await;
        cache.retain(|_, entry| {
            entry.created.elapsed() < Duration::from_secs(300) || entry.response.borrow().is_none()
        });
        let mut receiver = if let Some(entry) = cache.get(&request.request_id) {
            if entry.digest != digest {
                return Response::error(&request.request_id, "id_conflict", "相同ID包含不同命令");
            }
            entry.response.subscribe()
        } else {
            if cache.len() >= 1024 {
                return Response::error(
                    &request.request_id,
                    "busy",
                    "幂等请求缓存已满，请稍后重试",
                );
            }
            let (response, receiver) = watch::channel(None);
            cache.insert(
                request.request_id.clone(),
                Cached {
                    digest: digest.clone(),
                    created: Instant::now(),
                    response: response.clone(),
                },
            );
            let owner = self.clone();
            let id = request.request_id.clone();
            let command = request.command;
            let mut workers = self.workers.lock().await;
            while workers.try_join_next().is_some() {}
            workers.spawn(async move {
                let _serial = owner.mutations.lock().await;
                let result = match owner.execute(command, &id, &digest).await {
                    Ok(v) => Response::ok(&id, v),
                    Err(e) => {
                        *owner.last_error.lock().await = Some(e.clone());
                        Response::error(&id, "command_failed", e)
                    }
                };
                response.send_replace(Some(result));
            });
            receiver
        };
        drop(cache);
        let response =
            tokio::time::timeout(Duration::from_millis(request.timeout_ms.into()), async {
                loop {
                    if let Some(response) = receiver.borrow().clone() {
                        return Some(response);
                    }
                    if receiver.changed().await.is_err() {
                        return None;
                    }
                }
            })
            .await;
        match response {
            Ok(Some(response)) => response,
            _ => Response::error(
                &request.request_id,
                "outcome_unknown",
                "变更仍可能完成；使用相同请求ID重试或查询状态",
            ),
        }
    }
}

/// The caller initializes node.json explicitly before running this host.
pub async fn run(paths: AgentPaths) -> io::Result<()> {
    let _directory_lock =
        super::instance_lock::InstanceLock::acquire(&paths.lock()).map_err(io::Error::other)?;
    let record = ConfigRecord::load(&paths.config()).map_err(io::Error::other)?;
    let identity = Identity::load(&paths.identity()).map_err(io::Error::other)?;
    let _identity_lock = AgentPaths::identity_lock(identity.node_id())?;
    super::private_access::ensure_directory(&record.settings.receive_root)?;
    let mut session = record.settings.session().map_err(io::Error::other)?;
    session.tunnel_rules.clear();
    let (control, mut raw) = control::spawn_with_storage(
        identity,
        session,
        &paths.tasks(),
        record.settings.receive_root.clone(),
    )
    .map_err(io::Error::other)?;
    let (_, mut events) = control.subscribe().map_err(io::Error::other)?;
    let agent = Arc::new(Agent {
        control,
        config: Mutex::new(record),
        paths: paths.clone(),
        epoch: format!("{:032x}", rand::random::<u128>()),
        journal: Mutex::new(Journal {
            events: VecDeque::new(),
            floor: 0,
        }),
        cache: Mutex::new(HashMap::new()),
        mutations: Mutex::new(()),
        workers: Mutex::new(JoinSet::new()),
        last_error: Mutex::new(None),
        forwards: Mutex::new(super::agent_forward::ForwardManager::default()),
    });
    let mut listener = super::ipc_transport::Listener::bind(&paths)?;
    {
        let record = agent.config.lock().await;
        let mut forwards = agent.forwards.lock().await;
        let plan = forwards
            .prepare(&record.settings, &agent.control, None)
            .await
            .map_err(io::Error::other)?;
        let _ = forwards.commit(plan);
    }

    let collector = agent.clone();
    let drain = tokio::spawn(async move {
        loop {
            tokio::select! {
                raw=raw.recv()=>{if raw.is_none(){break;}},
                event=events.recv()=>{match event {
                    Ok(event)=>{let mut journal=collector.journal.lock().await;if journal.events.len()==256{journal.events.pop_front();}journal.events.push_back(event);},
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_))=>{let mut journal=collector.journal.lock().await;journal.events.clear();journal.floor=collector.control.snapshot().map(|s|s.sequence).unwrap_or(u64::MAX);},
                    Err(_)=>break,
                }}
            }
        }
    });
    let (stop, mut stopping) = watch::channel(false);
    let slots = Arc::new(Semaphore::new(32));
    let mut connections = JoinSet::new();
    let shutdown = shutdown_signal();
    tokio::pin!(shutdown);
    let reload = reload_signal();
    tokio::pin!(reload);
    let outcome = loop {
        tokio::select! {
            _=stopping.changed()=>break Ok(()),
            signal=&mut shutdown=>break signal,
            signal=&mut reload=>{
                if let Err(error)=signal {break Err(error);}
                reload.set(reload_signal());
                if let Ok(permit)=slots.clone().try_acquire_owned() {
                    let agent=agent.clone();
                    connections.spawn(async move {
                        let _permit=permit;
                        match read_candidate(&agent.paths.root.join("node.pending.json")) {
                            Ok(request)=>{let result=agent.submit(request).await;if let Some(error)=result.error{*agent.last_error.lock().await=Some(error.message);}},
                            Err(error)=>{*agent.last_error.lock().await=Some(error.to_string());}
                        }
                    });
                }
            },
            accepted=listener.accept()=>{
                let mut stream=match accepted { Ok(stream)=>stream, Err(error)=>break Err(error) };
                let Ok(permit)=slots.clone().try_acquire_owned() else {continue;};
                let agent=agent.clone();let stop=stop.clone();
                connections.spawn(async move {
                    let _permit=permit;
                    let request=tokio::time::timeout(Duration::from_secs(5),ipc::read::<Request>(&mut *stream)).await;
                    if let Ok(Ok(request))=request {
                        let stopping=matches!(request.command,Command::Stop);
                        let response=agent.submit(request).await;
                        let success=response.error.is_none();
                        let _=tokio::time::timeout(Duration::from_secs(5),ipc::write(&mut *stream,&response)).await;
                        if stopping && success {let _=stop.send(true);}
                    }
                });
            },
            _=connections.join_next(),if !connections.is_empty()=>{},
        }
    };
    drop(listener);
    connections.abort_all();
    while connections.join_next().await.is_some() {}
    let mut workers = agent.workers.lock().await;
    workers.abort_all();
    while workers.join_next().await.is_some() {}
    drop(workers);
    agent.forwards.lock().await.shutdown().await;
    agent.control.shutdown();
    let control = agent.control.clone();
    tokio::task::spawn_blocking(move || control.shutdown_and_wait())
        .await
        .map_err(io::Error::other)?;
    drain.abort();
    let _ = drain.await;
    outcome
}
async fn shutdown_signal() -> io::Result<()> {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! {result=tokio::signal::ctrl_c()=>result,_=terminate.recv()=>Ok(())}
    }
    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c().await
    }
}

async fn reload_signal() -> io::Result<()> {
    #[cfg(unix)]
    {
        let mut signal = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())?;
        signal.recv().await;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        std::future::pending().await
    }
}
fn read_candidate(path: &std::path::Path) -> io::Result<Request> {
    use std::io::Read;
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() > ipc::FRAME_LIMIT as u64
    {
        return Err(io::Error::other("候选必须是最大1MiB的普通文件"));
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(ipc::FRAME_LIMIT as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > ipc::FRAME_LIMIT {
        return Err(io::Error::other("候选过大"));
    }
    let request: Request = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
    request.validate()?;
    if !matches!(request.command, Command::ApplyConfig { .. }) {
        return Err(io::Error::other("SIGHUP候选只能是apply_config请求"));
    }
    Ok(request)
}
