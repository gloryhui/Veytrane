//! Multi-listener client supervisor.
//!
//! One [`DirectLink`] (one peer, one signaling registration) is shared by every
//! local forwarding rule. Rules can be added, removed or retargeted while the
//! process keeps running; changing one rule does not touch another listener or
//! any already accepted connection.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use tokio::net::TcpListener;
use tokio::sync::{Mutex, watch};
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

use crate::error::{Error, Result};
use crate::identity::Identity;
use crate::net::DirectLink;

use super::open_tunnel;
use super::rules::TunnelRule;

/// A local connection may wait this long for a fresh authenticated session.
const SESSION_CONNECT_TIMEOUT: Duration = Duration::from_secs(20);

/// Shared authenticated peer session. Concurrent callers serialize one connect
/// and then reuse the same QUIC connection.
pub struct SharedSession {
    link: Arc<DirectLink>,
    identity: Identity,
    connection: Mutex<Option<quinn::Connection>>,
}

impl SharedSession {
    pub fn new(link: Arc<DirectLink>, identity: Identity) -> Self {
        Self {
            link,
            identity,
            connection: Mutex::new(None),
        }
    }

    pub async fn connection(&self) -> Result<quinn::Connection> {
        let mut state = self.connection.lock().await;
        if let Some(connection) = state
            .as_ref()
            .filter(|connection| connection.close_reason().is_none())
        {
            return Ok(connection.clone());
        }
        let connection = self.link.connect_authenticated(&self.identity).await?;
        info!(
            peer = %self.link.peer_node_id.short(),
            remote = %connection.remote_address(),
            path = %self.link.network.transport_label(&connection),
            "共享会话已重新认证"
        );
        *state = Some(connection.clone());
        Ok(connection)
    }

    pub fn peer(&self) -> crate::identity::NodeId {
        self.link.peer_node_id
    }
}

struct RuleHandle {
    rule: TunnelRule,
    bound: SocketAddr,
    shutdown: watch::Sender<bool>,
    target: watch::Sender<SocketAddr>,
    task: JoinHandle<()>,
}

struct PreparedListener {
    rule: TunnelRule,
    listener: TcpListener,
    bound: SocketAddr,
}

/// Bound candidates are inert until commit; dropping releases every socket.
pub(crate) struct PreparedRules {
    desired: HashMap<String, TunnelRule>,
    additions: HashMap<String, PreparedListener>,
}

/// Owns one listener task per enabled rule and applies config reloads.
pub struct RuleSupervisor {
    session: Arc<SharedSession>,
    rules: HashMap<String, RuleHandle>,
}

impl RuleSupervisor {
    pub async fn start(session: Arc<SharedSession>, rules: &[TunnelRule]) -> Result<Self> {
        let mut supervisor = Self {
            session,
            rules: HashMap::new(),
        };
        for rule in rules.iter().filter(|rule| rule.enabled) {
            match supervisor.spawn_rule(rule).await {
                Ok(handle) => {
                    info!(
                        rule = %rule.id,
                        name = %rule.name,
                        listen = %handle.bound,
                        target = %rule.target,
                        "本地转发规则已启动"
                    );
                    supervisor.rules.insert(rule.id.clone(), handle);
                }
                Err(error) => {
                    supervisor.shutdown().await;
                    return Err(error);
                }
            }
        }
        Ok(supervisor)
    }

    /// A failed preparation leaves the complete previous rule set untouched.
    pub async fn apply(&mut self, rules: &[TunnelRule]) -> Vec<String> {
        match self.prepare(rules).await {
            Ok(prepared) => {
                let stopped = self.commit(prepared);
                for task in stopped {
                    let _ = task.await;
                }
                Vec::new()
            }
            Err(error) => vec![error.to_string()],
        }
    }

    pub(crate) async fn prepare(&self, rules: &[TunnelRule]) -> Result<PreparedRules> {
        let mut desired = HashMap::new();
        let mut additions = HashMap::new();
        for rule in rules.iter().filter(|rule| rule.enabled) {
            if desired.insert(rule.id.clone(), rule.clone()).is_some() {
                return Err(Error::Protocol(format!("重复规则 ID：{}", rule.id)));
            }
            if self
                .rules
                .get(&rule.id)
                .is_some_and(|old| old.rule.listen == rule.listen)
            {
                continue;
            }
            if rule.listen.port() != 0
                && let Some((id, _)) = self.rules.iter().find(|(_, old)| old.bound == rule.listen)
            {
                return Err(Error::Protocol(format!(
                    "监听地址 {} 仍由规则 {id} 占用；请先停用该规则，再分步修改",
                    rule.listen
                )));
            }
            let candidate = Self::prepare_listener(rule).await?;
            additions.insert(rule.id.clone(), candidate);
        }
        Ok(PreparedRules { desired, additions })
    }

    /// The caller holds the supervisor lock from prepare through commit. This
    /// section deliberately has no await or fallible resource acquisition.
    pub(crate) fn commit(&mut self, prepared: PreparedRules) -> Vec<JoinHandle<()>> {
        let PreparedRules { desired, additions } = prepared;
        let removed: Vec<_> = self
            .rules
            .keys()
            .filter(|id| !desired.contains_key(*id) || additions.contains_key(*id))
            .cloned()
            .collect();
        let mut stopped = Vec::new();
        for id in removed {
            if let Some(old) = self.rules.remove(&id) {
                let _ = old.shutdown.send(true);
                stopped.push(old.task);
            }
        }
        for (id, rule) in desired {
            if let Some(existing) = self.rules.get_mut(&id) {
                existing.target.send_replace(rule.target);
                existing.rule = rule;
            }
        }
        for (id, candidate) in additions {
            let handle = self.activate(candidate);
            info!(rule = %id, listen = %handle.bound, "本地转发规则已提交");
            self.rules.insert(id, handle);
        }
        stopped
    }

    pub async fn shutdown(&mut self) {
        let handles: Vec<RuleHandle> = self.rules.drain().map(|(_, handle)| handle).collect();
        for handle in handles {
            stop_handle(handle).await;
        }
    }

    /// Bound listener addresses, for diagnostics and tests.
    pub fn listeners(&self) -> Vec<(String, SocketAddr)> {
        let mut listeners: Vec<_> = self
            .rules
            .iter()
            .map(|(id, handle)| (id.clone(), handle.bound))
            .collect();
        listeners.sort_by(|left, right| left.0.cmp(&right.0));
        listeners
    }

    pub fn peer(&self) -> crate::identity::NodeId {
        self.session.peer()
    }

    async fn prepare_listener(rule: &TunnelRule) -> Result<PreparedListener> {
        let listener = TcpListener::bind(rule.listen).await.map_err(|error| {
            Error::Transport(format!("监听本机地址 {} 失败: {error}", rule.listen))
        })?;
        let bound = listener.local_addr()?;
        Ok(PreparedListener {
            rule: rule.clone(),
            listener,
            bound,
        })
    }

    async fn spawn_rule(&self, rule: &TunnelRule) -> Result<RuleHandle> {
        Ok(self.activate(Self::prepare_listener(rule).await?))
    }

    fn activate(&self, candidate: PreparedListener) -> RuleHandle {
        let PreparedListener {
            rule,
            listener,
            bound,
        } = candidate;
        let (shutdown, shutdown_rx) = watch::channel(false);
        let (target, target_rx) = watch::channel(rule.target);
        let session = self.session.clone();
        let id = rule.id.clone();
        let task = tokio::spawn(run_rule(id, listener, target_rx, session, shutdown_rx));
        RuleHandle {
            rule,
            bound,
            shutdown,
            target,
            task,
        }
    }
}

async fn stop_handle(handle: RuleHandle) {
    let _ = handle.shutdown.send(true);
    let _ = handle.task.await;
}

async fn run_rule(
    id: String,
    listener: TcpListener,
    mut target: watch::Receiver<SocketAddr>,
    session: Arc<SharedSession>,
    mut shutdown: watch::Receiver<bool>,
) {
    loop {
        let accepted = tokio::select! {
            biased;
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    break;
                }
                continue;
            }
            accepted = listener.accept() => accepted,
        };
        let (tcp, from) = match accepted {
            Ok(accepted) => accepted,
            Err(error) => {
                warn!(rule = %id, %error, "接受本地连接失败");
                tokio::time::sleep(Duration::from_millis(50)).await;
                continue;
            }
        };
        let target = *target.borrow_and_update();
        let session = session.clone();
        let rule = id.clone();
        tokio::spawn(async move {
            let connection = match tokio::time::timeout(
                SESSION_CONNECT_TIMEOUT,
                session.connection(),
            )
            .await
            {
                Ok(Ok(connection)) => connection,
                Ok(Err(error)) => {
                    warn!(rule = %rule, %from, %target, %error, "共享会话建立失败，本地连接已关闭");
                    return;
                }
                Err(_) => {
                    warn!(rule = %rule, %from, %target, "共享会话建立超时，本地连接已关闭");
                    return;
                }
            };
            if let Err(error) = open_tunnel(tcp, &connection, &target.to_string()).await {
                debug!(rule = %rule, %from, %target, %error, "本地转发结束");
            }
        });
    }
}
