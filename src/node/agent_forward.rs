//! Agent-owned listeners. Preparing is inert; committed route updates affect
//! only future accepts. Established streams are owned by the node task group.
use super::{agent_config::NodeSettings, control::NodeControl};
use crate::{
    error::{Error, Result},
    identity::NodeId,
};
use std::{collections::HashMap, net::SocketAddr, sync::Arc, time::Duration};
use tokio::{
    net::TcpListener,
    sync::{Semaphore, watch},
    task::JoinHandle,
};

#[derive(Clone)]
struct Route {
    connection: watch::Receiver<Option<quinn::Connection>>,
    target: SocketAddr,
    service: Option<String>,
}
struct Running {
    listen: SocketAddr,
    route: watch::Sender<Route>,
    stop: watch::Sender<bool>,
    task: JoinHandle<()>,
    error: Arc<std::sync::Mutex<Option<String>>>,
}
pub(crate) struct Prepared {
    desired: HashMap<String, (SocketAddr, Route)>,
    additions: HashMap<String, TcpListener>,
}
pub(crate) struct ForwardManager {
    active: HashMap<String, Running>,
    owner: Arc<crate::tunnel::ForwardStreamOwner>,
}
impl Default for ForwardManager {
    fn default() -> Self {
        Self {
            active: HashMap::new(),
            owner: Arc::new(crate::tunnel::ForwardStreamOwner::new(256)),
        }
    }
}
impl ForwardManager {
    pub async fn prepare(
        &self,
        settings: &NodeSettings,
        control: &NodeControl,
        start: Option<&str>,
    ) -> Result<Prepared> {
        let mut plan = Prepared {
            desired: HashMap::new(),
            additions: HashMap::new(),
        };
        for rule in settings.forwards.iter().filter(|r| {
            r.enabled
                && (r.auto_start || self.active.contains_key(&r.id) || start == Some(r.id.as_str()))
        }) {
            let peer = NodeId::from_hex(&rule.peer_node_id)?;
            let connection = control
                .connection_updates(peer)
                .await
                .map_err(Error::Protocol)?;
            let service = if settings.peer_modes.get(&rule.peer_node_id)
                == Some(&super::wire::PeerWireMode::LegacyDesktopV1)
            {
                None
            } else {
                Some(
                    settings
                        .service_ids
                        .get(&rule.id)
                        .ok_or_else(|| Error::Protocol("缺少服务ID".into()))?
                        .clone(),
                )
            };
            let route = Route {
                connection,
                target: rule.target,
                service,
            };
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
            let slots = Arc::new(Semaphore::new(32));
            let status_error = error.clone();
            let task = tokio::spawn(async move {
                loop {
                    tokio::select! {biased;
                        _=stopped.changed()=>break,
                        changed=current.changed()=>{if changed.is_err(){break;}},
                        accepted=listener.accept()=>{
                            let (tcp,_)=match accepted{Ok(value)=>value,Err(e)=>{*errors.lock().unwrap()=Some(e.to_string());break;}};
                            let Ok(permit)=slots.clone().try_acquire_owned()else{*errors.lock().unwrap()=Some("单规则TCP并发已满".into());continue;};
                            let route=current.borrow().clone();let errors=errors.clone();
                            if !owner.spawn(async move {
                                let _permit=permit;
                                let result=open(tcp,route).await;
                                if let Err(e)=result{*errors.lock().unwrap()=Some(e.to_string());}
                            }){*error.lock().unwrap()=Some("节点TCP并发已满或正在退出".into());}
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
                    error: status_error,
                },
            );
        }
        retired
    }
    pub async fn stop(&mut self, id: &str) -> Result<()> {
        if let Some(old) = self.active.remove(id) {
            old.stop.send_replace(true);
            old.task.await.map_err(|e| Error::Protocol(e.to_string()))?;
        }
        Ok(())
    }
    pub fn snapshot(&self) -> serde_json::Value {
        serde_json::json!({"active_streams":self.owner.active(),"rules":self.active.iter().map(|(id,r)|serde_json::json!({"id":id,"listen":r.listen,"running":!r.task.is_finished(),"last_error":*r.error.lock().unwrap()})).collect::<Vec<_>>()})
    }
    pub async fn shutdown(&mut self) {
        for (_, old) in self.active.drain() {
            old.stop.send_replace(true);
            let _ = old.task.await;
        }
        self.owner.shutdown().await;
    }
}
async fn open(tcp: tokio::net::TcpStream, mut route: Route) -> Result<()> {
    let connection = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let Some(connection) = route
                .connection
                .borrow()
                .as_ref()
                .filter(|c| c.close_reason().is_none())
                .cloned()
            {
                return Ok::<_, Error>(connection);
            }
            route
                .connection
                .changed()
                .await
                .map_err(|_| Error::Transport("节点连接已停止".into()))?;
        }
    })
    .await
    .map_err(|_| Error::Transport("等待对端认证超时，请先连接设备".into()))??;
    if let Some(service) = route.service {
        let stream = super::services::open_tcp(&connection, &service).await?;
        crate::tunnel::bridge_tcp(tcp, stream.send, stream.recv).await
    } else {
        super::tunnel::open_tunnel(tcp, &connection, route.target).await
    }
}
