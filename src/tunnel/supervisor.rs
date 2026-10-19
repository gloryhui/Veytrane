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

    /// Apply a validated rule set. Invalid/busy rules keep the previous listener
    /// and are reported instead of taking down unrelated rules.
    pub async fn apply(&mut self, rules: &[TunnelRule]) -> Vec<String> {
        let mut errors = Vec::new();
        let desired: HashMap<String, TunnelRule> = rules
            .iter()
            .filter(|rule| rule.enabled)
            .cloned()
            .map(|rule| (rule.id.clone(), rule))
            .collect();

        let removed: Vec<String> = self
            .rules
            .keys()
            .filter(|id| !desired.contains_key(*id))
            .cloned()
            .collect();
        for id in removed {
            if let Some(handle) = self.rules.remove(&id) {
                info!(rule = %id, "本地转发规则已停止");
                stop_handle(handle).await;
            }
        }

        for (id, rule) in desired {
            if let Some(existing) = self.rules.get(&id)
                && existing.rule.listen == rule.listen
            {
                let retarget = existing.rule.target != rule.target;
                if let Some(existing) = self.rules.get_mut(&id) {
                    if retarget {
                        existing.target.send_replace(rule.target);
                        info!(rule = %id, listen = %existing.bound, target = %rule.target, "本地转发目标已更新");
                    }
                    existing.rule = rule;
                }
                continue;
            }

            let conflict = self
                .rules
                .iter()
                .find(|(other, handle)| *other != &id && handle.bound == rule.listen)
                .map(|(other, _)| other.clone());
            if let Some(other) = conflict {
                errors.push(format!(
                    "规则 {id} 的监听地址 {} 已被规则 {other} 占用，保持原规则不变",
                    rule.listen
                ));
                continue;
            }

            let old = self.rules.remove(&id);
            match self.spawn_rule(&rule).await {
                Ok(handle) => {
                    if let Some(old) = old {
                        stop_handle(old).await;
                    }
                    info!(
                        rule = %id,
                        name = %rule.name,
                        listen = %handle.bound,
                        target = %rule.target,
                        "本地转发规则已启动"
                    );
                    self.rules.insert(id, handle);
                }
                Err(error) => {
                    errors.push(format!("规则 {id} 启动失败，保持原规则不变：{error}"));
                    if let Some(old) = old {
                        self.rules.insert(id, old);
                    }
                }
            }
        }

        errors
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

    async fn spawn_rule(&self, rule: &TunnelRule) -> Result<RuleHandle> {
        let listener = TcpListener::bind(rule.listen).await.map_err(|error| {
            Error::Transport(format!("监听本机地址 {} 失败: {error}", rule.listen))
        })?;
        let bound = listener.local_addr()?;
        let (shutdown, shutdown_rx) = watch::channel(false);
        let (target, target_rx) = watch::channel(rule.target);
        let session = self.session.clone();
        let id = rule.id.clone();
        let task = tokio::spawn(run_rule(id, listener, target_rx, session, shutdown_rx));
        Ok(RuleHandle {
            rule: rule.clone(),
            bound,
            shutdown,
            target,
            task,
        })
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
