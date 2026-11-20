//! Transactional TCP listeners. Accepted flows outlive individual rule stops.

use crate::{
    DEFAULT_MAX_NODE_TCP_STREAMS, DEFAULT_MAX_TUNNEL_STREAMS, ForwardStreamOwner, bridge_tcp,
    rules::TunnelRule,
};
use serde::Serialize;
use std::{
    collections::{HashMap, HashSet},
    future::Future,
    net::SocketAddr,
    sync::Arc,
    time::Duration,
};
use tokio::{
    net::TcpListener,
    sync::{Semaphore, watch},
    task::JoinHandle,
};
use veytrane_transport::{
    error::{Error, Result},
    identity::NodeId,
};

#[derive(Clone)]
pub struct PeerConnection {
    updates: watch::Receiver<Option<quinn::Connection>>,
    reconnect: Arc<dyn Fn() -> Result<()> + Send + Sync>,
}

impl PeerConnection {
    pub fn new(
        updates: watch::Receiver<Option<quinn::Connection>>,
        reconnect: impl Fn() -> Result<()> + Send + Sync + 'static,
    ) -> Self {
        Self {
            updates,
            reconnect: Arc::new(reconnect),
        }
    }
}

#[derive(Serialize)]
pub struct ForwardSnapshot {
    active_streams: usize,
    manually_stopped: HashSet<String>,
    rules: Vec<RuleStatus>,
}

#[derive(Serialize)]
struct RuleStatus {
    id: String,
    listen: SocketAddr,
    listening: bool,
    peer_ready: bool,
    last_error: Option<String>,
}

#[derive(Clone)]
struct Route {
    peer: PeerConnection,
    service: String,
}
struct Running {
    listen: SocketAddr,
    route: watch::Sender<Route>,
    stop: watch::Sender<bool>,
    task: JoinHandle<()>,
    error: Arc<std::sync::Mutex<Option<String>>>,
}
pub struct Prepared {
    known: HashSet<String>,
    forced_start: Option<String>,
    desired: HashMap<String, (SocketAddr, Route)>,
    additions: HashMap<String, TcpListener>,
}
pub struct ForwardManager {
    suppressed: HashSet<String>,
    active: HashMap<String, Running>,
    owner: Arc<ForwardStreamOwner>,
}
impl Default for ForwardManager {
    fn default() -> Self {
        Self {
            active: HashMap::new(),
            suppressed: HashSet::new(),
            owner: Arc::new(ForwardStreamOwner::new(DEFAULT_MAX_NODE_TCP_STREAMS)),
        }
    }
}
impl ForwardManager {
    pub async fn prepare<Resolve, ResolveFuture>(
        &self,
        rules: &[TunnelRule],
        service_ids: &HashMap<String, String>,
        start: Option<&str>,
        resolve: Resolve,
    ) -> Result<Prepared>
    where
        Resolve: Fn(NodeId) -> ResolveFuture,
        ResolveFuture: Future<Output = Result<PeerConnection>>,
    {
        let mut plan = Prepared {
            known: rules.iter().map(|r| r.id.clone()).collect(),
            forced_start: start.map(str::to_owned),
            desired: HashMap::new(),
            additions: HashMap::new(),
        };
        for rule in rules.iter().filter(|r| {
            r.enabled
                && (start == Some(r.id.as_str())
                    || (!self.suppressed.contains(&r.id)
                        && (r.auto_start || self.active.contains_key(&r.id))))
        }) {
            let peer = resolve(NodeId::from_hex(&rule.peer_node_id)?).await?;
            let service = service_ids
                .get(&rule.id)
                .ok_or_else(|| Error::Protocol("转发缺少服务 ID".into()))?
                .clone();
            let route = Route { peer, service };
            if self
                .active
                .get(&rule.id)
                .is_none_or(|old| old.listen != rule.listen || old.task.is_finished())
            {
                // A port swap cannot be prepared while its previous listener owns it.
                plan.additions
                    .insert(rule.id.clone(), TcpListener::bind(rule.listen).await?);
            }
            plan.desired.insert(rule.id.clone(), (rule.listen, route));
        }
        Ok(plan)
    }
    pub fn commit(&mut self, mut plan: Prepared) -> Vec<JoinHandle<()>> {
        self.suppressed.retain(|id| plan.known.contains(id));
        if let Some(id) = &plan.forced_start {
            self.suppressed.remove(id);
        }
        let mut retired = vec![];
        let removed = self
            .active
            .keys()
            .filter(|id| !plan.desired.contains_key(*id) || plan.additions.contains_key(*id))
            .cloned()
            .collect::<Vec<_>>();
        for id in removed {
            if let Some(old) = self.active.remove(&id) {
                old.stop.send_replace(true);
                retired.push(old.task);
            }
        }
        for (id, (listen, route)) in plan.desired {
            if let Some(old) = self.active.get(&id) {
                old.route.send_replace(route);
                *old.error.lock().unwrap() = None;
                continue;
            }
            let listener = plan
                .additions
                .remove(&id)
                .expect("all new listeners are prepared");
            let (routes, mut current) = watch::channel(route);
            let (stop, mut stopped) = watch::channel(false);
            let error = Arc::new(std::sync::Mutex::new(None));
            let errors = error.clone();
            let owner = self.owner.clone();
            let slots = Arc::new(Semaphore::new(DEFAULT_MAX_TUNNEL_STREAMS));
            let task = tokio::spawn(async move {
                loop {
                    tokio::select! {
                        biased;
                        _ = stopped.changed() => break,
                        changed = current.changed() => {
                            if changed.is_err() {
                                break;
                            }
                        }
                        accepted = listener.accept() => {
                            let (tcp, _) = match accepted {
                                Ok(connection) => connection,
                                Err(error) => {
                                    *errors.lock().unwrap() =
                                        Some(error.to_string().chars().take(256).collect());
                                    break;
                                }
                            };
                            let Ok(permit) = slots.clone().try_acquire_owned() else {
                                *errors.lock().unwrap() = Some("单规则TCP并发已满".into());
                                continue;
                            };
                            let route = current.borrow().clone();
                            let flow_errors = errors.clone();
                            if !owner.spawn(async move {
                                let _permit = permit;
                                if let Err(error) = open(tcp, route).await {
                                    *flow_errors.lock().unwrap() =
                                        Some(error.to_string().chars().take(256).collect());
                                }
                            }) {
                                *errors.lock().unwrap() = Some("节点TCP并发已满或正在退出".into());
                            }
                        }
                    }
                }
            });
            self.active.insert(
                id,
                Running {
                    listen,
                    route: routes,
                    stop,
                    task,
                    error,
                },
            );
        }
        retired
    }
    pub async fn stop(&mut self, id: &str) -> Result<()> {
        self.suppressed.insert(id.to_owned());
        if let Some(old) = self.active.remove(id) {
            old.stop.send_replace(true);
            old.task.await.map_err(|e| Error::Protocol(e.to_string()))?;
        }
        Ok(())
    }
    pub fn snapshot(&self) -> ForwardSnapshot {
        ForwardSnapshot {
            active_streams: self.owner.active(),
            manually_stopped: self.suppressed.clone(),
            rules: self
                .active
                .iter()
                .map(|(id, running)| RuleStatus {
                    id: id.clone(),
                    listen: running.listen,
                    listening: !running.task.is_finished(),
                    peer_ready: running
                        .route
                        .borrow()
                        .peer
                        .updates
                        .borrow()
                        .as_ref()
                        .is_some_and(|connection| connection.close_reason().is_none()),
                    last_error: running.error.lock().unwrap().clone(),
                })
                .collect(),
        }
    }
    pub async fn shutdown(&mut self) {
        for (_, old) in self.active.drain() {
            old.stop.send_replace(true);
            let _ = old.task.await;
        }
        self.owner.shutdown().await;
    }
}
async fn open(tcp: tokio::net::TcpStream, route: Route) -> Result<()> {
    let mut updates = route.peer.updates;
    if updates
        .borrow()
        .as_ref()
        .is_none_or(|c| c.close_reason().is_some())
    {
        (route.peer.reconnect)()?;
    }
    let connection = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let Some(connection) = updates
                .borrow()
                .as_ref()
                .filter(|c| c.close_reason().is_none())
                .cloned()
            {
                return Ok::<_, Error>(connection);
            }
            updates
                .changed()
                .await
                .map_err(|_| Error::Transport("节点连接已停止".into()))?;
        }
    })
    .await
    .map_err(|_| Error::Transport("等待对端认证超时，请先连接设备".into()))??;
    let stream = crate::services::open_tcp(&connection, &route.service).await?;
    bridge_tcp(tcp, stream.send, stream.recv).await
}
