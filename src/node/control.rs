//! In-process node control. Commands and snapshots contain no GUI or QUIC handles.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, mpsc};

pub use super::drop_send::Admission as PathAdmission;
use super::remote_auth::SecretPassword;
pub use super::session::{SessionEvent, change_trusted_device};
use super::task_model::TaskId;
use crate::error::{Error, Result};
use crate::identity::{Identity, NodeId};

pub use super::session::{
    NodeSessionConfig, NodeSessionHandle as NodeControl, TrustedDeviceChange,
};

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct PeerSnapshot {
    pub node_id: String,
    pub generation: u64,
    pub phase: String,
    pub inbound_password: bool,
    pub inbound_trusted: bool,
    pub outbound_password: bool,
    pub outbound_trusted: bool,
    pub inbound_trust_source: Option<super::authorization::TrustSource>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct NodeSnapshot {
    pub configuration_revision: u64,
    pub sequence: u64,
    pub node_id: String,
    pub signal: String,
    pub peers: BTreeMap<String, PeerSnapshot>,
    pub tunnels: BTreeMap<String, String>,
    pub short_id: Option<String>,
    #[serde(default)]
    pub short_peers: BTreeMap<String, String>,
    pub truncated: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NodeEvent {
    Signal {
        phase: String,
    },
    Peer {
        snapshot: PeerSnapshot,
    },
    Tunnel {
        rule_id: String,
        phase: String,
    },
    Identity {
        node_id: String,
        short_id: Option<String>,
    },
    Changed,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SequencedEvent {
    pub sequence: u64,
    pub event: NodeEvent,
}

#[derive(Clone)]
pub(crate) struct ControlState {
    snapshot: Arc<Mutex<NodeSnapshot>>,
    events: broadcast::Sender<SequencedEvent>,
    trust_sources: Arc<Mutex<BTreeMap<String, super::authorization::TrustSource>>>,
    pub(crate) transfer: Option<super::transfer::TransferService>,
}

impl ControlState {
    pub(crate) fn set_trust_sources(
        &self,
        sources: &std::collections::HashMap<NodeId, super::authorization::TrustSource>,
    ) {
        if let Ok(mut current) = self.trust_sources.lock() {
            *current = sources
                .iter()
                .map(|(id, source)| (id.to_hex(), *source))
                .collect();
        }
    }
    pub(crate) fn record_trust_source(
        &self,
        peer: NodeId,
        source: super::authorization::TrustSource,
    ) {
        if let Ok(mut current) = self.trust_sources.lock() {
            current.insert(peer.to_hex(), source);
        }
    }
    pub(crate) fn retain_trust_sources(&self, devices: &[super::trusted_devices::TrustedDevice]) {
        if let Ok(mut sources) = self.trust_sources.lock() {
            sources.retain(|id, _| devices.iter().any(|device| &device.node_id == id));
        }
    }
    pub(crate) fn new(
        identity: NodeId,
        transfer: Option<super::transfer::TransferService>,
    ) -> Self {
        let (events, _) = broadcast::channel(256);
        Self {
            snapshot: Arc::new(Mutex::new(NodeSnapshot {
                node_id: identity.to_hex(),
                signal: "unconfigured".into(),
                ..Default::default()
            })),
            events,
            trust_sources: Default::default(),
            transfer,
        }
    }

    pub(crate) fn observe(&self, event: &SessionEvent) {
        use super::network_state::{NetworkLifecycle as N, PeerLifecycle as P};
        let Ok(mut state) = self.snapshot.lock() else {
            return;
        };
        let change = match event {
            SessionEvent::Lifecycle(lifecycle) => {
                let phase = match lifecycle {
                    N::Unconfigured => Some("unconfigured"),
                    N::ConnectingSignal => Some("connecting"),
                    N::SignalOnline => Some("online"),
                    N::ReconnectingSignal { .. } => Some("reconnecting"),
                    N::Failed { .. } | N::Disconnected { peer: None, .. } => Some("failed"),
                    _ => None,
                };
                if let Some(phase) = phase {
                    state.signal = phase.into();
                    NodeEvent::Signal {
                        phase: phase.into(),
                    }
                } else {
                    NodeEvent::Changed
                }
            }
            SessionEvent::PeerState {
                peer,
                generation,
                state: lifecycle,
            } => {
                let id = peer.to_hex();
                if state
                    .peers
                    .get(&id)
                    .is_some_and(|old| old.generation > *generation)
                {
                    return;
                }
                if !state.peers.contains_key(&id)
                    && state.peers.len() >= super::network_state::MAX_PEERS
                {
                    let retired = state
                        .peers
                        .iter()
                        .filter(|(_, p)| p.phase != "connected")
                        .min_by_key(|(_, p)| p.generation)
                        .map(|(id, _)| id.clone());
                    if let Some(id) = retired {
                        state.peers.remove(&id);
                    } else {
                        state.truncated = true;
                        return;
                    }
                }
                let mut peer = PeerSnapshot {
                    node_id: id.clone(),
                    generation: *generation,
                    ..Default::default()
                };
                peer.phase = match lifecycle {
                    P::PeerPending => "pending",
                    P::Punching => "punching",
                    P::Authenticating => "authenticating",
                    P::Negotiating => "negotiating",
                    P::RemoteAuthPending => "authorization_pending",
                    P::Connected(_) => "connected",
                    P::Disconnected => "disconnected",
                    P::Failed(_) => "failed",
                }
                .into();
                if let P::Connected(auth) = lifecycle {
                    peer.inbound_password = auth.inbound.password;
                    peer.inbound_trusted = auth.inbound.trusted_device;
                    if peer.inbound_trusted {
                        peer.inbound_trust_source = Some(
                            self.trust_sources
                                .lock()
                                .ok()
                                .and_then(|sources| sources.get(&id).copied())
                                .unwrap_or_default(),
                        );
                    }
                    peer.outbound_password = auth.outbound.password;
                    peer.outbound_trusted = auth.outbound.trusted_device;
                }
                state.peers.insert(id, peer.clone());
                NodeEvent::Peer { snapshot: peer }
            }
            SessionEvent::TunnelState {
                rule_id,
                state: runtime,
            } => {
                use super::session::TunnelRuntimeState as T;
                let phase = match runtime {
                    T::WaitingAuthorization => "waiting_authorization",
                    T::Starting => "starting",
                    T::Running => "running",
                    T::Stopped => "stopped",
                    T::Error(_) => "failed",
                }
                .to_owned();
                if state.tunnels.contains_key(rule_id) || state.tunnels.len() < 512 {
                    state.tunnels.insert(rule_id.clone(), phase.clone());
                } else {
                    state.truncated = true;
                }
                NodeEvent::Tunnel {
                    rule_id: rule_id.clone(),
                    phase,
                }
            }
            SessionEvent::ShortIdRegistered(id) => {
                state.short_id = Some(id.to_string());
                NodeEvent::Identity {
                    node_id: state.node_id.clone(),
                    short_id: state.short_id.clone(),
                }
            }
            SessionEvent::ShortIdResolved { short_id, peer } => {
                let key = short_id.to_string();
                if let Some(peer) = peer {
                    if state.short_peers.len() < super::network_state::MAX_PEERS
                        || state.short_peers.contains_key(&key)
                    {
                        state.short_peers.insert(key, peer.to_hex());
                    }
                } else {
                    state.short_peers.remove(&key);
                }
                NodeEvent::Changed
            }
            _ => NodeEvent::Changed,
        };
        let Some(sequence) = state.sequence.checked_add(1) else {
            state.truncated = true;
            return;
        };
        state.sequence = sequence;
        // Publish under the snapshot lock so subscribe+snapshot cannot miss a gap.
        let _ = self.events.send(SequencedEvent {
            sequence,
            event: change,
        });
    }

    fn subscribe(&self) -> Result<(NodeSnapshot, broadcast::Receiver<SequencedEvent>)> {
        let snapshot = self
            .snapshot
            .lock()
            .map_err(|_| Error::Protocol("节点快照锁不可用".into()))?;
        let receiver = self.events.subscribe();
        Ok((snapshot.clone(), receiver))
    }
}

/// The original bounded stream is retained for the transitional desktop adapter.
/// Other managers subscribe to sequenced events and resync after Lagged.
pub struct ControlEvents {
    receiver: mpsc::Receiver<SessionEvent>,
}
impl ControlEvents {
    pub async fn recv(&mut self) -> Option<SessionEvent> {
        self.receiver.recv().await
    }
    pub async fn drain(&mut self) {
        while self.receiver.recv().await.is_some() {}
    }
}

pub fn spawn(
    identity: Identity,
    config: NodeSessionConfig,
) -> std::io::Result<(NodeControl, ControlEvents)> {
    let (handle, receiver) = super::session::spawn(identity, config)?;
    Ok((handle, ControlEvents { receiver }))
}

/// Explicit persistent file service; capability negotiation is finalized in P3.
pub fn spawn_with_storage(
    identity: Identity,
    mut config: NodeSessionConfig,
    tasks: &Path,
    receive_root: PathBuf,
) -> Result<(NodeControl, ControlEvents)> {
    let (store, _) =
        super::task_store::TaskStore::open(tasks).map_err(|e| Error::Protocol(e.to_string()))?;
    config.transfer = Some(super::transfer::TransferService::new(store, receive_root));
    spawn(identity, config).map_err(Error::Io)
}

pub enum NodeCommand {
    Connect {
        peer: NodeId,
        password: SecretPassword,
    },
    ConnectTrusted {
        peer: NodeId,
    },
    StartTunnel {
        rule_id: String,
    },
    StopTunnel {
        rule_id: String,
    },
    SendPaths {
        peer: NodeId,
        paths: Vec<PathBuf>,
    },
    Pause {
        task: TaskId,
    },
    Resume {
        peer: NodeId,
        task: TaskId,
    },
    ConfirmDirectory {
        peer: NodeId,
        request: TaskId,
    },
    CancelDirectory {
        request: TaskId,
    },
    StartSpeed {
        peer: NodeId,
        direction: super::config::SpeedtestDirection,
        seconds: u16,
    },
    StopSpeed {
        peer: NodeId,
        task: TaskId,
    },
    Stop,
}

impl NodeControl {
    pub(crate) fn configuration_committed(&self, revision: u64) -> Result<()> {
        let mut snapshot = self
            .control
            .snapshot
            .lock()
            .map_err(|_| Error::Protocol("节点快照锁不可用".into()))?;
        snapshot.configuration_revision = revision;
        snapshot.sequence = snapshot
            .sequence
            .checked_add(1)
            .ok_or_else(|| Error::Protocol("事件序号耗尽".into()))?;
        let _ = self.control.events.send(SequencedEvent {
            sequence: snapshot.sequence,
            event: NodeEvent::Changed,
        });
        Ok(())
    }
    pub fn snapshot(&self) -> Result<NodeSnapshot> {
        self.control.subscribe().map(|(s, _)| s)
    }
    pub fn subscribe(&self) -> Result<(NodeSnapshot, broadcast::Receiver<SequencedEvent>)> {
        self.control.subscribe()
    }
    pub fn execute(&self, command: NodeCommand) -> Result<()> {
        let result = match command {
            NodeCommand::Connect { peer, password } => {
                self.connect_peer_with_password(peer, password)
            }
            NodeCommand::ConnectTrusted { peer } => self.connect_peer_trusted(peer),
            NodeCommand::StartTunnel { rule_id } => self.start_tunnel_rule(rule_id),
            NodeCommand::StopTunnel { rule_id } => self.stop_tunnel_rule(rule_id),
            NodeCommand::SendPaths { peer, paths } => self.send_paths(peer, paths),
            NodeCommand::Pause { task } => self.pause_task(task),
            NodeCommand::Resume { peer, task } => self.resume_task(peer, task),
            NodeCommand::ConfirmDirectory { peer, request } => {
                self.confirm_directory(peer, request)
            }
            NodeCommand::CancelDirectory { request } => self.cancel_directory_preview(request),
            NodeCommand::StartSpeed {
                peer,
                direction,
                seconds,
            } => self.start_speed(peer, direction, seconds),
            NodeCommand::StopSpeed { peer, task } => self.cancel_speed(peer, task),
            NodeCommand::Stop => {
                self.shutdown();
                Ok(())
            }
        };
        result.map_err(Error::Protocol)
    }
    pub fn task_snapshot(&self) -> Result<Option<super::snapshot::Snapshot>> {
        self.control
            .transfer
            .as_ref()
            .map(|service| service.ui_snapshot())
            .transpose()
    }
}

impl NodeControl {
    fn file_service(&self) -> Result<&super::transfer::TransferService> {
        self.control
            .transfer
            .as_ref()
            .ok_or_else(|| Error::Protocol("文件服务未启用".into()))
    }
    pub async fn cancel_task(&self, task: TaskId) -> Result<()> {
        self.file_service()?.cancel_task(task).await
    }
    pub fn set_auto_resume(&self, enabled: bool) -> Result<()> {
        self.file_service()?.set_auto_resume(enabled);
        Ok(())
    }
    pub fn set_send_limit(&self, limit: u8) -> Result<()> {
        self.file_service()?.set_send_limit(limit)
    }
    pub fn set_file_limits(&self, limits: super::bandwidth::FileLimits) -> Result<()> {
        self.file_service()?.set_file_limits(limits)
    }
    pub fn set_receive_root(&self, root: PathBuf) -> Result<()> {
        self.file_service()?.set_receive_root(root);
        Ok(())
    }
    pub fn set_receive_safety_mib(&self, value: u16) -> Result<()> {
        self.file_service()?.receive_space.set_safety_mib(value)
    }
    pub fn remove_completed_task(&self, task: &TaskId) -> Result<()> {
        self.file_service()?.remove_completed_task(task)
    }
    pub fn remove_completed_history(
        &self,
        tasks: std::collections::BTreeSet<TaskId>,
    ) -> Result<Vec<TaskId>> {
        self.file_service()?.remove_completed_history(tasks)
    }
    pub fn delete_completed_receive_file(&self, task: &TaskId) -> Result<()> {
        self.file_service()?.delete_completed_receive_file(task)
    }
    pub fn select_detail(&self, selection: Option<super::task_details::Selection>) -> Result<()> {
        self.file_service()?.select_detail(selection);
        Ok(())
    }
    pub fn reveal_task(&self, selection: &super::task_details::Selection) -> Result<PathBuf> {
        self.file_service()?.reveal_task(selection)
    }
}
