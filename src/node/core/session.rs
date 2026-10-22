//! Long-lived headless node network session. One task owns signaling events; Quinn owns all UDP reads.

use std::collections::{HashMap, HashSet, VecDeque};
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::Poll;
use std::time::Duration;

use tokio::sync::{Semaphore, mpsc, oneshot, watch};
use tokio::task::JoinSet;
use tokio::time::{self, Sleep};
use tracing::{debug, warn};

use crate::discovery::signal::{Candidate, CandidateKind, SignalMessage, SignalingClient};
use crate::error::{Error, Result};
use crate::identity::{Identity, NodeId};
use crate::nat::punch::{PunchConfig, PunchToken};
use crate::net::race::{ConnectionGuard, PreparedTransport};
use crate::net::{
    AddressFamily, DesktopNetwork, DesktopNetworkConfig, NetworkPath, prepare_desktop_network,
};
#[cfg(test)]
use crate::transport::handshake::handshake_initiator;
use crate::transport::handshake::handshake_responder;
#[cfg(test)]
use crate::transport::quic::connect as quic_connect;
use crate::transport::quic::{
    ACCEPT_FIRST_BI_STREAM_TIMEOUT, APPLICATION_HANDSHAKE_TIMEOUT, ChannelBinding,
    QUIC_HANDSHAKE_TIMEOUT,
};

use crate::discovery::short_id::ShortId;
use crate::node::core::remote_auth::{
    AuthContext, RemoteAuthorization, RemoteVerifier, SecretPassword,
};

use crate::node::core::network_state::{
    BeginPeerAttempt, MAX_PEERS, MAX_PENDING_PEERS, NetworkLifecycle, PeerLifecycle, PeerRegistry,
    reconnect_delay, should_initiate_quic,
};

mod direct;
#[cfg(test)]
mod direct_tests;
mod recovery;

const COMMAND_CAPACITY: usize = 64;
const EVENT_CAPACITY: usize = 128;
const SESSION_INPUT_CAPACITY: usize = 64;
const MAX_CANDIDATES: usize = crate::discovery::signal::MAX_CANDIDATES;
const MAX_RECONNECT_PROBES_PER_PEER: usize = 2;
// Authenticated transport hint only: resumption still requires fresh session
// authorization and the existing opt-in task recovery policy.
const NETWORK_REFRESH_CLOSE_CODE: u32 = 0x4e52;
// Bound waiting across signaling, punching and application authentication.
const TUNNEL_PEER_WAIT_TIMEOUT: Duration = Duration::from_secs(60);
const PEER_PUNCH_CONFIG: PunchConfig = PunchConfig {
    interval: Duration::from_millis(200),
    attempts: 30,
};

fn is_current_signal_generation(current: u64, result_generation: u64) -> bool {
    current == result_generation
}

#[derive(Clone, Debug)]
pub struct NodeSessionConfig {
    pub direct: crate::node::core::connectivity::DirectSettings,
    pub service_ids: HashMap<String, String>,
    pub resources: crate::node::core::authorization::ResourcePolicy,
    pub trust_sources: HashMap<NodeId, crate::node::core::authorization::TrustSource>,
    pub enable_files: bool,
    pub enable_speed: bool,
    pub enable_tcp: bool,
    pub signal_server: String,
    pub signal_tls: Option<crate::discovery::signal_tls::SignalTlsClient>,
    pub remote_auth: Option<RemoteVerifier>,
    pub trusted_devices: Vec<crate::node::core::trusted_devices::TrustedDevice>,
    pub config_path: Option<std::path::PathBuf>,
    #[cfg(test)]
    pub test_outgoing_password: Option<SecretPassword>,
    pub network: DesktopNetworkConfig,
    pub allowed_forward_targets: Vec<crate::node::core::config::AllowedForwardTarget>,
    pub tunnel_rules: Vec<crate::node::core::config::TunnelRule>,
    pub(crate) transfer: Option<crate::node::core::transfer::TransferService>,
}

impl NodeSessionConfig {
    pub fn new(signal_server: impl Into<String>) -> Self {
        Self {
            direct: crate::node::core::connectivity::DirectSettings {
                enabled: false,
                ..Default::default()
            },
            service_ids: HashMap::new(),
            resources: Default::default(),
            trust_sources: HashMap::new(),
            enable_files: true,
            enable_speed: true,
            enable_tcp: true,
            signal_server: signal_server.into(),
            signal_tls: None,
            remote_auth: None,
            trusted_devices: Vec::new(),
            config_path: None,
            #[cfg(test)]
            test_outgoing_password: None,
            network: DesktopNetworkConfig::default(),
            allowed_forward_targets: Vec::new(),
            tunnel_rules: Vec::new(),
            transfer: None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TunnelRuntimeState {
    WaitingAuthorization,
    Starting,
    Running,
    Stopped,
    Error(String),
}

#[derive(Clone, Debug)]
pub enum SessionEvent {
    Connectivity(crate::node::core::connectivity::ConnectivitySnapshot),
    SignalSecurity {
        tls: bool,
    },
    NetworkPaths(String),
    NetworkFamilies {
        ipv4: bool,
        ipv6: bool,
        relay_configured: bool,
    },
    PeerCandidates {
        peer: NodeId,
        generation: u64,
        ipv4: usize,
        ipv6: usize,
    },
    PeerTransport {
        peer: NodeId,
        generation: u64,
        relay: bool,
        family: AddressFamily,
    },
    PeerPath {
        peer: NodeId,
        generation: u64,
        detail: String,
    },
    TrustedDevicesChanged(Vec<crate::node::core::trusted_devices::TrustedDevice>),
    Lifecycle(NetworkLifecycle),
    PeerState {
        peer: NodeId,
        generation: u64,
        state: PeerLifecycle,
    },
    SignalIdentityRegistered(NodeId),
    ShortIdRegistered(ShortId),
    ShortIdResolved {
        short_id: ShortId,
        peer: Option<NodeId>,
    },
    Diagnostic(String),
    DirectoryPreview(crate::node::core::incremental::Preview),
    DirectoryPreviewFailed(crate::node::core::task_model::TaskId),
    SelectionQueued {
        peer: NodeId,
        count: usize,
    },
    SpeedRequestEnded {
        peer: NodeId,
    },
    SpeedPhaseCompleted {
        peer: NodeId,
        snapshot: crate::node::core::speed::SpeedSnapshot,
    },
    TunnelState {
        rule_id: String,
        state: TunnelRuntimeState,
    },
    TunnelError {
        rule_id: String,
        detail: String,
    },
}

#[derive(Clone)]
struct SessionEvents {
    sender: mpsc::Sender<SessionEvent>,
    shutdown: watch::Receiver<bool>,
    control: crate::node::core::control::ControlState,
    recovery: Arc<std::sync::Mutex<recovery::Recovery>>,
}

impl SessionEvents {
    fn new(
        sender: mpsc::Sender<SessionEvent>,
        shutdown: watch::Receiver<bool>,
        control: crate::node::core::control::ControlState,
    ) -> Self {
        Self {
            sender,
            shutdown,
            control,
            recovery: Default::default(),
        }
    }

    async fn send(&self, event: SessionEvent) -> bool {
        let mut shutdown = self.shutdown.clone();
        if *shutdown.borrow() {
            return false;
        }
        let mut event = Some(event);
        tokio::select! {
            biased;
            _ = shutdown.changed() => false,
            permit = self.sender.reserve() => match permit {
                Ok(permit) => {
                    let event = event.take().expect("event is sent only once");
                    self.recovery.lock().unwrap().event(&event);
                    self.control.observe(&event);
                    permit.send(event);
                    true
                }
                Err(_) => false,
            }
        }
    }
}

#[derive(Clone)]
pub struct NodeSessionHandle {
    pub(crate) control: crate::node::core::control::ControlState,
    resources: watch::Sender<crate::node::core::authorization::ResourcePolicy>,
    commands: mpsc::Sender<SessionCommand>,
    allowed_forward_targets: watch::Sender<Vec<crate::node::core::config::AllowedForwardTarget>>,
    lifetime: Arc<SessionLifetime>,
}

struct SessionLifetime {
    stop: watch::Sender<bool>,
    done: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
    thread: std::sync::Mutex<Option<std::thread::JoinHandle<()>>>,
}

impl Drop for SessionLifetime {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
        // Bound application exit even if an OS operation fails to unwind.
        if self
            .done
            .get_mut()
            .unwrap()
            .recv_timeout(Duration::from_secs(5))
            .is_ok()
            && let Some(thread) = self.thread.get_mut().unwrap().take()
        {
            let _ = thread.join();
        }
    }
}

impl NodeSessionHandle {
    /// Internal listener demand; unlike an explicit user connect this does not
    /// clear manual disconnect fences or invent a credential.
    pub(crate) fn request_peer_connection(&self, peer: NodeId) -> std::result::Result<(), String> {
        self.commands
            .try_send(SessionCommand::ConnectPeer(peer))
            .map_err(|_| "重连请求队列已满或节点已停止".into())
    }
    pub(crate) async fn connection_updates(
        &self,
        peer: NodeId,
    ) -> std::result::Result<watch::Receiver<Option<quinn::Connection>>, String> {
        let (done, result) = oneshot::channel();
        self.commands
            .send(SessionCommand::ConnectionUpdates { peer, done })
            .await
            .map_err(|_| "会话已关闭".to_owned())?;
        result.await.map_err(|_| "会话已关闭".to_owned())?
    }
    pub(crate) async fn commit_agent_policy(
        &self,
        record: crate::node::core::settings::ConfigRecord,
        path: std::path::PathBuf,
        options: crate::node::core::settings::CommitOptions,
    ) -> std::result::Result<crate::storage::atomic::WriteOutcome, String> {
        let (done, result) = oneshot::channel();
        self.commands
            .send(SessionCommand::CommitAgentPolicy {
                record: Box::new(record),
                path,
                options,
                done,
            })
            .await
            .map_err(|_| "会话已关闭，配置未入队".to_owned())?;
        result
            .await
            .map_err(|_| "recovery_required: 配置结果未知，须按磁盘根重启恢复".to_owned())?
    }
    pub async fn list_services(
        &self,
        peer: NodeId,
    ) -> std::result::Result<Vec<crate::node::core::wire::ServiceDescriptor>, String> {
        let (done, result) = oneshot::channel();
        tokio::time::timeout(Duration::from_secs(15), async {
            self.commands
                .send(SessionCommand::ListServices { peer, done })
                .await
                .map_err(|_| "节点已停止".to_owned())?;
            result.await.map_err(|_| "节点已停止".to_owned())?
        })
        .await
        .map_err(|_| "服务目录查询超时".to_owned())?
    }
    pub async fn disconnect_peer(&self, peer: NodeId) -> std::result::Result<(), String> {
        let (done, result) = oneshot::channel();
        self.commands
            .send(SessionCommand::DisconnectPeer { peer, done })
            .await
            .map_err(|_| "节点已停止".to_owned())?;
        result.await.map_err(|_| "节点已停止".to_owned())?
    }
    pub async fn apply_resource_policy(
        &self,
        policy: crate::node::core::authorization::ResourcePolicy,
    ) -> std::result::Result<(), String> {
        policy.validate().map_err(|error| error.to_string())?;
        let (done, result) = oneshot::channel();
        self.commands
            .send(SessionCommand::ResourcePolicy { policy, done })
            .await
            .map_err(|_| "节点已停止".to_owned())?;
        result.await.map_err(|_| "节点已停止".to_owned())
    }
    pub fn resource_policy(&self) -> crate::node::core::authorization::ResourcePolicy {
        self.resources.borrow().clone()
    }
    #[cfg(test)]
    pub fn connect_peer(&self, peer: NodeId) -> std::result::Result<(), String> {
        self.connect_peer_with_password(peer, test_password())
    }
    pub async fn connect_address(
        &self,
        peer: NodeId,
        addresses: Vec<SocketAddr>,
        password: Option<SecretPassword>,
    ) -> std::result::Result<(), String> {
        crate::node::core::connectivity::validate_addresses(&addresses)
            .map_err(|e| e.to_string())?;
        let (done, result) = oneshot::channel();
        self.commands
            .send(SessionCommand::ConnectAddress {
                peer,
                addresses,
                password,
                done,
            })
            .await
            .map_err(|_| "节点已停止".to_owned())?;
        result.await.map_err(|_| "节点已停止".to_owned())?
    }
    pub fn connect_peer_with_password(
        &self,
        peer: NodeId,
        password: SecretPassword,
    ) -> std::result::Result<(), String> {
        self.commands
            .try_send(SessionCommand::ConnectAuthenticated { peer, password })
            .map_err(|_| "网络会话暂时无法接收连接请求".into())
    }
    pub fn connect_short_id(
        &self,
        short_id: ShortId,
        password: SecretPassword,
    ) -> std::result::Result<(), String> {
        self.commands
            .try_send(SessionCommand::ConnectShort {
                short_id,
                password: Some(password),
            })
            .map_err(|_| "网络会话暂时无法接收短 ID 查询".into())
    }
    pub fn connect_peer_trusted(&self, peer: NodeId) -> std::result::Result<(), String> {
        self.commands
            .try_send(SessionCommand::ConnectTrusted(peer))
            .map_err(|_| "连接请求队列不可用".into())
    }
    pub fn connect_short_id_trusted(&self, short_id: ShortId) -> std::result::Result<(), String> {
        self.commands
            .try_send(SessionCommand::ConnectShort {
                short_id,
                password: None,
            })
            .map_err(|_| "短 ID 查询队列不可用".into())
    }
    pub async fn change_trusted_device(
        &self,
        change: TrustedDeviceChange,
    ) -> std::result::Result<Vec<crate::node::core::trusted_devices::TrustedDevice>, String> {
        let (done, result) = oneshot::channel();
        self.commands
            .send(SessionCommand::ChangeTrustedDevice { change, done })
            .await
            .map_err(|_| "会话已关闭".to_owned())?;
        result.await.map_err(|_| "会话已关闭".to_owned())?
    }
    pub async fn update_remote_auth(
        &self,
        verifier: RemoteVerifier,
    ) -> std::result::Result<(), String> {
        let (done, finished) = oneshot::channel();
        self.commands
            .send(SessionCommand::UpdateRemoteAuth { verifier, done })
            .await
            .map_err(|_| "网络会话已经关闭".to_owned())?;
        finished.await.map_err(|_| "网络会话已经关闭".into())
    }

    pub fn reconfigure_network(
        &self,
        server: String,
        relay_server: Option<String>,
        signal_tls: Option<crate::discovery::signal_tls::SignalTlsClient>,
    ) -> std::result::Result<(), String> {
        if let Some(spec) = &relay_server {
            crate::relay::client::validate_server_spec(spec).map_err(|e| e.to_string())?;
        }
        self.commands
            .try_send(SessionCommand::ReconfigureNetwork {
                server,
                relay_server,
                signal_tls,
            })
            .map_err(|e| format!("网络配置队列不可用：{e}"))
    }

    pub fn start_tunnel_rule(&self, rule_id: impl Into<String>) -> std::result::Result<(), String> {
        self.commands
            .try_send(SessionCommand::StartTunnel(rule_id.into()))
            .map_err(|error| format!("无法启动本机转发规则：{error}"))
    }

    pub fn stop_tunnel_rule(&self, rule_id: impl Into<String>) -> std::result::Result<(), String> {
        self.commands
            .try_send(SessionCommand::StopTunnel(rule_id.into()))
            .map_err(|error| format!("无法停止本机转发规则：{error}"))
    }

    pub fn update_tunnel_settings(
        &self,
        allowed_forward_targets: Vec<crate::node::core::config::AllowedForwardTarget>,
        tunnel_rules: Vec<crate::node::core::config::TunnelRule>,
    ) -> std::result::Result<(), String> {
        self.commands
            .try_send(SessionCommand::UpdateTunnelSettings { tunnel_rules })
            .map_err(|error| format!("无法应用端口转发设置：{error}"))?;
        self.allowed_forward_targets
            .send_replace(allowed_forward_targets);
        Ok(())
    }

    /// Apply only revocations from an unsaved draft; grants require successful persistence.
    pub fn restrict_forward_targets(
        &self,
        draft: &[crate::node::core::config::AllowedForwardTarget],
    ) {
        self.allowed_forward_targets.send_modify(|applied| {
            applied.retain_mut(|entry| {
                let Some(next) = draft.iter().find(|next| {
                    next.id == entry.id && next.enabled && next.target == entry.target
                }) else {
                    return false;
                };
                entry
                    .allowed_peers
                    .retain(|peer| next.allowed_peers.contains(peer));
                !entry.allowed_peers.is_empty()
            });
        });
    }

    /// Complete only after the listener task has drained and released its socket.
    pub async fn revoke_tunnel_rule(
        &self,
        rule_id: String,
        delete: bool,
    ) -> std::result::Result<(), String> {
        let (done, stopped) = oneshot::channel();
        self.commands
            .send(SessionCommand::RevokeTunnel {
                rule_id,
                delete,
                done,
            })
            .await
            .map_err(|_| "网络会话已关闭".to_owned())?;
        stopped
            .await
            .map_err(|_| "网络会话在停止规则前退出".to_owned())
    }

    pub fn send_paths(
        &self,
        peer: NodeId,
        sources: Vec<std::path::PathBuf>,
    ) -> std::result::Result<(), String> {
        let sources =
            crate::node::core::drop_send::normalize(sources).map_err(|error| error.to_string())?;
        self.commands
            .try_send(SessionCommand::SendPaths { peer, sources })
            .map_err(|_| "传输命令队列已满或会话已关闭；拖入项尚未提交".into())
    }
    #[allow(dead_code)] // T010 product controls call this tested backend command.
    pub fn send_file(
        &self,
        peer: NodeId,
        source: std::path::PathBuf,
    ) -> std::result::Result<(), String> {
        self.commands
            .try_send(SessionCommand::SendFile { peer, source })
            .map_err(|_| "传输命令队列已满或会话已关闭".into())
    }
    #[allow(dead_code)] // T010 product controls call this tested backend command.
    pub fn pause_task(
        &self,
        id: crate::node::core::task_model::TaskId,
    ) -> std::result::Result<(), String> {
        self.commands
            .try_send(SessionCommand::PauseTask(id))
            .map_err(|_| "传输命令队列已满或会话已关闭".into())
    }
    #[allow(dead_code)] // T010 product controls call this tested backend command.
    pub fn resume_task(
        &self,
        peer: NodeId,
        id: crate::node::core::task_model::TaskId,
    ) -> std::result::Result<(), String> {
        self.commands
            .try_send(SessionCommand::ResumeTask { peer, id })
            .map_err(|_| "传输命令队列已满或会话已关闭".into())
    }

    #[allow(dead_code)] // T010 selection controls.
    pub fn preview_directory(
        &self,
        peer: NodeId,
        source: std::path::PathBuf,
    ) -> std::result::Result<crate::node::core::task_model::TaskId, String> {
        let request = crate::node::core::task_model::TaskId::generate();
        self.commands
            .try_send(SessionCommand::PreviewDirectory {
                peer,
                source,
                request: request.clone(),
            })
            .map_err(|_| "目录预览命令队列已满或会话已关闭".to_owned())?;
        Ok(request)
    }
    pub fn confirm_directory(
        &self,
        peer: NodeId,
        id: crate::node::core::task_model::TaskId,
    ) -> std::result::Result<(), String> {
        self.commands
            .try_send(SessionCommand::ConfirmDirectory { peer, id })
            .map_err(|_| "目录确认命令队列已满或会话已关闭".into())
    }
    pub fn cancel_directory_preview(
        &self,
        id: crate::node::core::task_model::TaskId,
    ) -> std::result::Result<(), String> {
        self.commands
            .try_send(SessionCommand::CancelDirectoryPreview(id))
            .map_err(|_| "目录取消命令队列已满或会话已关闭".into())
    }

    pub fn send_directory(
        &self,
        peer: NodeId,
        source: std::path::PathBuf,
    ) -> std::result::Result<(), String> {
        self.commands
            .try_send(SessionCommand::SendDirectory { peer, source })
            .map_err(|_| "传输命令队列已满或会话已关闭".into())
    }
    #[allow(dead_code)] // T010 controls.
    pub fn start_speed(
        &self,
        peer: NodeId,
        direction: crate::node::core::config::SpeedtestDirection,
        seconds: u16,
    ) -> std::result::Result<(), String> {
        self.commands
            .try_send(SessionCommand::StartSpeed {
                peer,
                direction,
                seconds,
            })
            .map_err(|_| "测速命令队列已满或会话已关闭".into())
    }
    #[allow(dead_code)] // T010 controls.
    pub fn cancel_speed(
        &self,
        peer: NodeId,
        id: crate::node::core::task_model::TaskId,
    ) -> std::result::Result<(), String> {
        self.commands
            .try_send(SessionCommand::CancelSpeed { peer, id })
            .map_err(|_| "测速命令队列已满或会话已关闭".into())
    }
    pub fn is_running(&self) -> bool {
        !self.commands.is_closed()
    }

    pub fn shutdown(&self) {
        let _ = self.lifetime.stop.send(true);
    }

    /// Wait off the UI thread for structured task cleanup and durable checkpoints.
    pub fn shutdown_and_wait(&self) {
        self.shutdown();
        if self
            .lifetime
            .done
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(5))
            .is_ok()
            && let Some(thread) = self.lifetime.thread.lock().unwrap().take()
        {
            let _ = thread.join();
        }
    }
}

#[derive(Clone)]
pub enum TrustedDeviceChange {
    Trust {
        peer: NodeId,
        generation: u64,
        display_name: String,
    },
    Rename {
        peer: NodeId,
        display_name: String,
    },
    Revoke {
        peer: NodeId,
    },
}

/// Shell mutations use a live actor for runtime revocation, or the latest
/// durable configuration after the command channel has closed. Trust is live-only.
pub async fn change_trusted_device(
    session: Option<&NodeSessionHandle>,
    path: &std::path::Path,
    change: TrustedDeviceChange,
) -> std::result::Result<Vec<crate::node::core::trusted_devices::TrustedDevice>, String> {
    if let Some(session) = session
        && session.is_running()
    {
        let result = session.change_trusted_device(change.clone()).await;
        if result.is_ok()
            || session.is_running()
            || matches!(change, TrustedDeviceChange::Trust { .. })
        {
            return result;
        }
        // Shutdown may close the channel after the running check. Only a dead
        // actor permits the offline path; a live actor's errors stay failures.
    }
    match change {
        TrustedDeviceChange::Trust { .. } => Err("当前认证会话不可用，未信任设备".into()),
        TrustedDeviceChange::Revoke { peer } => {
            crate::node::core::config::DesktopConfig::revoke_trusted_device(path, peer)
                .map_err(|e| e.to_string())
        }
        TrustedDeviceChange::Rename { peer, display_name } => {
            crate::node::core::config::DesktopConfig::rename_trusted_device(
                path,
                peer,
                display_name,
            )
            .map_err(|e| e.to_string())
        }
    }
}
enum SessionCommand {
    ConnectAddress {
        peer: NodeId,
        addresses: Vec<SocketAddr>,
        password: Option<SecretPassword>,
        done: oneshot::Sender<std::result::Result<(), String>>,
    },
    ListServices {
        peer: NodeId,
        done: oneshot::Sender<
            std::result::Result<Vec<crate::node::core::wire::ServiceDescriptor>, String>,
        >,
    },
    DisconnectPeer {
        peer: NodeId,
        done: oneshot::Sender<std::result::Result<(), String>>,
    },
    ResourcePolicy {
        policy: crate::node::core::authorization::ResourcePolicy,
        done: oneshot::Sender<()>,
    },
    RetryCandidates {
        peer: NodeId,
        candidates: Vec<Candidate>,
        token: PunchToken,
    },
    ChangeTrustedDevice {
        change: TrustedDeviceChange,
        done: oneshot::Sender<
            std::result::Result<Vec<crate::node::core::trusted_devices::TrustedDevice>, String>,
        >,
    },
    StartSpeed {
        peer: NodeId,
        direction: crate::node::core::config::SpeedtestDirection,
        seconds: u16,
    },
    CancelSpeed {
        peer: NodeId,
        id: crate::node::core::task_model::TaskId,
    },
    PreviewDirectory {
        peer: NodeId,
        source: std::path::PathBuf,
        request: crate::node::core::task_model::TaskId,
    },
    ConfirmDirectory {
        peer: NodeId,
        id: crate::node::core::task_model::TaskId,
    },
    CancelDirectoryPreview(crate::node::core::task_model::TaskId),
    SendPaths {
        peer: NodeId,
        sources: Vec<std::path::PathBuf>,
    },
    #[allow(dead_code)] // T010 selection controls.
    SendDirectory {
        peer: NodeId,
        source: std::path::PathBuf,
    },
    #[allow(dead_code)] // T010 product controls.
    SendFile {
        peer: NodeId,
        source: std::path::PathBuf,
    },
    #[allow(dead_code)] // T010 product controls.
    PauseTask(crate::node::core::task_model::TaskId),
    #[allow(dead_code)] // T010 product controls.
    ResumeTask {
        peer: NodeId,
        id: crate::node::core::task_model::TaskId,
    },
    ConnectPeer(NodeId),
    ConnectTrusted(NodeId),
    ConnectAuthenticated {
        peer: NodeId,
        password: SecretPassword,
    },
    ConnectShort {
        short_id: ShortId,
        password: Option<SecretPassword>,
    },
    UpdateRemoteAuth {
        verifier: RemoteVerifier,
        done: oneshot::Sender<()>,
    },
    RefreshNetwork(&'static str),
    ReconfigureNetwork {
        server: String,
        relay_server: Option<String>,
        signal_tls: Option<crate::discovery::signal_tls::SignalTlsClient>,
    },
    StartTunnel(String),
    StopTunnel(String),
    RevokeTunnel {
        rule_id: String,
        delete: bool,
        done: oneshot::Sender<()>,
    },
    ConnectionUpdates {
        peer: NodeId,
        done: oneshot::Sender<
            std::result::Result<watch::Receiver<Option<quinn::Connection>>, String>,
        >,
    },
    CommitAgentPolicy {
        record: Box<crate::node::core::settings::ConfigRecord>,
        path: std::path::PathBuf,
        options: crate::node::core::settings::CommitOptions,
        done: oneshot::Sender<std::result::Result<crate::storage::atomic::WriteOutcome, String>>,
    },
    UpdateTunnelSettings {
        tunnel_rules: Vec<crate::node::core::config::TunnelRule>,
    },
    #[cfg(test)]
    TestNetworkLoss(NodeId, oneshot::Sender<()>),
    #[cfg(test)]
    InspectDeferred(NodeId, oneshot::Sender<Option<PunchToken>>),
    #[cfg(test)]
    Inspect(oneshot::Sender<HashMap<NodeId, (u64, quinn::Connection)>>),
}

impl SessionCommand {
    fn required_capability(&self) -> u64 {
        match self {
            Self::StartSpeed { .. } => crate::node::core::protocol::CAP_SPEED_EXECUTION,
            Self::PreviewDirectory { .. }
            | Self::ConfirmDirectory { .. }
            | Self::SendDirectory { .. } => crate::node::core::protocol::CAP_DIRECTORY_TRANSFER,
            _ => crate::node::core::protocol::CAP_FILE_TRANSFER,
        }
    }
    fn preview_request(&self) -> Option<crate::node::core::task_model::TaskId> {
        match self {
            Self::PreviewDirectory { request, .. } => Some(request.clone()),
            _ => None,
        }
    }
    fn business_peer(&self) -> Option<NodeId> {
        match self {
            Self::PreviewDirectory { peer, .. }
            | Self::ConfirmDirectory { peer, .. }
            | Self::SendPaths { peer, .. }
            | Self::SendFile { peer, .. }
            | Self::SendDirectory { peer, .. }
            | Self::ResumeTask { peer, .. }
            | Self::StartSpeed { peer, .. } => Some(*peer),
            _ => None,
        }
    }
}

enum SessionInput {
    DirectIdentified {
        peer: NodeId,
        prepared: PreparedTransport,
        permit: tokio::sync::OwnedSemaphorePermit,
    },
    AuthorizationChanged {
        peer: NodeId,
        generation: u64,
        transport: usize,
    },
    Incoming(AddressFamily, Box<quinn::Incoming>),
    SignalConnected {
        generation: u64,
        result: Result<SignalingClient>,
    },
    PeerPath {
        peer: NodeId,
        generation: u64,
        detail: String,
    },
    PeerProgress {
        peer: NodeId,
        generation: u64,
        state: PeerLifecycle,
    },
    PeerConnected {
        peer: NodeId,
        generation: u64,
        capabilities: u64,
        authorization: RemoteAuthorization,
        control: Box<crate::node::core::remote_auth::TrustedControl>,
        connection: quinn::Connection,
    },
    PeerFailed {
        peer: NodeId,
        generation: u64,
        detail: String,
    },
    PeerClosed {
        peer: NodeId,
        generation: u64,
        detail: String,
        network_interruption: bool,
    },
}

// A lookup alone must preserve a live connection. A matching fresh probe is
// evidence that the peer is actually establishing a new transport. These watches
// use Quinn's existing dispatcher, never a second UDP receive owner.
struct ReconnectProbe {
    generation: u64,
    candidates: Vec<Candidate>,
    token: PunchToken,
    receivers: Vec<crate::transport::quic::PunchProbeReceiver>,
    deadline: Pin<Box<Sleep>>,
}
fn poll_reconnect_probes(
    probes: &mut HashMap<NodeId, VecDeque<ReconnectProbe>>,
    cx: &mut std::task::Context<'_>,
) -> Poll<(NodeId, usize, bool)> {
    for (peer, watches) in probes {
        for (index, watch) in watches.iter_mut().enumerate() {
            for receiver in &mut watch.receivers {
                if let Poll::Ready(Some(_)) = receiver.poll_recv(cx) {
                    return Poll::Ready((*peer, index, true));
                }
            }
            if watch.deadline.as_mut().poll(cx).is_ready() {
                return Poll::Ready((*peer, index, false));
            }
        }
    }
    Poll::Pending
}

struct RunningTunnel {
    rule: crate::node::core::config::TunnelRule,
    shutdown: watch::Sender<bool>,
}

type TunnelTaskResult = (String, std::result::Result<(), String>);
type TunnelTaskJoin = Option<std::result::Result<TunnelTaskResult, tokio::task::JoinError>>;

/// Start a dedicated Tokio runtime so GPUI's UI thread never performs network work.
pub fn spawn(
    identity: Identity,
    config: NodeSessionConfig,
) -> std::io::Result<(NodeSessionHandle, mpsc::Receiver<SessionEvent>)> {
    if config.service_ids.len() > 512 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "本机服务映射数量超限",
        ));
    }
    for (rule, service) in &config.service_ids {
        crate::node::core::wire::validate_service_id(rule)
            .and_then(|()| crate::node::core::wire::validate_service_id(service))
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    }
    config
        .resources
        .validate()
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    if config.trust_sources.len() > 128 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "节点策略设备数量超限",
        ));
    }
    crate::node::core::trusted_devices::validate(&config.trusted_devices)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let control =
        crate::node::core::control::ControlState::new(identity.node_id(), config.transfer.clone());
    control.set_trust_sources(&config.trust_sources);
    let runtime_control = control.clone();
    let (resources, _) = watch::channel(config.resources.clone());
    let runtime_resources = resources.clone();
    let (command_tx, command_rx) = mpsc::channel(COMMAND_CAPACITY);
    let session_command_tx = command_tx.clone();
    let (event_tx, event_rx) = mpsc::channel(EVENT_CAPACITY);
    let thread_events = event_tx.clone();
    let (allowed_forward_targets, _) = watch::channel(config.allowed_forward_targets.clone());
    let runtime_allowed_targets = allowed_forward_targets.clone();
    let (stop, stopped) = watch::channel(false);
    let event_shutdown = stopped.clone();
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let thread = std::thread::Builder::new()
        .name("p2p-node-network".into())
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build();
            match runtime {
                Ok(runtime) => {
                    runtime.block_on(async move {
                        let transfer = config.transfer.clone();
                        run_session(
                            identity,
                            config,
                            session_command_tx,
                            command_rx,
                            SessionPolicies {
                                targets: runtime_allowed_targets,
                                resources: runtime_resources,
                            },
                            stopped,
                            SessionEvents::new(event_tx.clone(), event_shutdown, runtime_control),
                        )
                        .await;
                        if let Some(transfer) = transfer {
                            let _ = transfer.interrupt_all().await;
                        }
                    });
                    runtime.shutdown_timeout(Duration::from_secs(1));
                }
                Err(error) => {
                    let _ = thread_events.blocking_send(SessionEvent::Lifecycle(
                        NetworkLifecycle::Failed {
                            detail: format!("创建网络运行时失败：{error}"),
                        },
                    ));
                }
            }
            let _ = done_tx.send(());
        })?;
    Ok((
        NodeSessionHandle {
            control,
            resources,
            commands: command_tx,
            allowed_forward_targets,
            lifetime: Arc::new(SessionLifetime {
                stop,
                done: std::sync::Mutex::new(done_rx),
                thread: std::sync::Mutex::new(Some(thread)),
            }),
        },
        event_rx,
    ))
}

struct SessionPolicies {
    targets: watch::Sender<Vec<crate::node::core::config::AllowedForwardTarget>>,
    resources: watch::Sender<crate::node::core::authorization::ResourcePolicy>,
}

async fn run_session(
    identity: Identity,
    config: NodeSessionConfig,
    command_tx: mpsc::Sender<SessionCommand>,
    mut commands: mpsc::Receiver<SessionCommand>,
    policies: SessionPolicies,
    mut shutdown: watch::Receiver<bool>,
    events: SessionEvents,
) {
    let SessionPolicies {
        targets: allowed_forward_targets,
        resources: resource_policy,
    } = policies;
    let _ = events
        .send(SessionEvent::SignalSecurity {
            tls: config.signal_tls.is_some(),
        })
        .await;
    let unconfigured = config.signal_server.trim().is_empty();
    emit_lifecycle(
        &events,
        if unconfigured && config.direct.enabled {
            NetworkLifecycle::LocalOnly
        } else if unconfigured {
            NetworkLifecycle::Unconfigured
        } else {
            NetworkLifecycle::ConnectingSignal
        },
    )
    .await;
    let mut preparation = config.network.clone();
    if unconfigured {
        // Signal-free mode must not query public STUN, Relay, or DNS services.
        preparation.stun_servers.clear();
        preparation.relay_server = None;
    }
    let prepared = if unconfigured && !config.direct.enabled {
        Ok(crate::net::DesktopNetwork {
            relay: None,
            paths: Vec::new(),
            local_candidates: Vec::new(),
            diagnostics: Vec::new(),
        })
    } else {
        prepare_desktop_network(&preparation).await
    };
    let mut network = match prepared {
        Ok(network) => network,
        Err(error) => {
            for rule in config
                .tunnel_rules
                .iter()
                .filter(|rule| rule.enabled && rule.auto_start)
            {
                let _ = events
                    .send(SessionEvent::TunnelState {
                        rule_id: rule.id.clone(),
                        state: TunnelRuntimeState::Error(error.to_string()),
                    })
                    .await;
            }
            emit_lifecycle(
                &events,
                NetworkLifecycle::Failed {
                    detail: error.to_string(),
                },
            )
            .await;
            return;
        }
    };

    let tunnel_policy = TunnelServices {
        services: config.service_ids.clone(),
    };
    let service_queries = Arc::new(Semaphore::new(16));
    let forward_streams = Arc::new(crate::tunnel::ForwardStreamOwner::new(256));
    let local_capabilities = crate::node::core::wire::capabilities(
        config.transfer.is_some() && config.enable_files,
        config.transfer.is_some() && config.enable_speed,
        config.enable_tcp,
    );
    let mut nearby = crate::node::core::connectivity::NearbyCache::new(identity.node_id());
    let (mut discovery, mut discovery_state) =
        if config.direct.enabled && config.direct.lan_discovery {
            match crate::node::core::connectivity::Discovery::start(identity.node_id(), &network) {
                Ok(d) => (Some(d), "active".to_owned()),
                Err(error) => {
                    warn!(%error, "局域网发现不可用，IP直连仍可使用");
                    (None, "unavailable".to_owned())
                }
            }
        } else {
            (None, "disabled".to_owned())
        };
    let mut connectivity = crate::node::core::connectivity::snapshot(
        &network,
        config.direct.enabled,
        discovery_state.clone(),
        &nearby,
    );
    let _ = events
        .send(SessionEvent::Connectivity(connectivity.clone()))
        .await;
    let mut direct_routes = config.direct.peers.clone();
    let mut configured_direct_routes = config.direct.peers.clone();
    let mut direct_dialing = HashSet::new();
    let direct_slots = Arc::new(Semaphore::new(MAX_PENDING_PEERS));
    let mut manually_disconnected = HashSet::new();
    let mut requested_peers = HashSet::new();
    let mut remote_verifier = config.remote_auth.clone();
    let auth_context = AuthContext::default();
    let mut trusted_devices = config.trusted_devices.clone();
    auth_context.set_trusted(&trusted_devices);
    let mut authorizations: HashMap<
        NodeId,
        (
            u64,
            usize,
            crate::node::core::remote_auth::AuthorizationPublisher,
        ),
    > = HashMap::new();
    let mut credentials: HashMap<NodeId, SecretPassword> = HashMap::new();
    let mut credential_retries = HashSet::new();
    let mut resolved_short_ids: HashMap<NodeId, ShortId> = HashMap::new();
    let mut short_queries: HashMap<ShortId, (Option<SecretPassword>, time::Instant)> =
        HashMap::new();
    let local_node = identity.node_id();
    let (mut input_tx, mut inputs) = mpsc::channel(SESSION_INPUT_CAPACITY);
    let mut session_tasks = JoinSet::new();
    let _ = events
        .send(SessionEvent::NetworkPaths(network.diagnostics.join("; ")))
        .await;
    let _ = events
        .send(SessionEvent::NetworkFamilies {
            ipv4: network.path(AddressFamily::Ipv4).is_some(),
            ipv6: network.path(AddressFamily::Ipv6).is_some(),
            relay_configured: network.relay.is_some(),
        })
        .await;
    for path in &network.paths {
        let endpoint = path.endpoint.clone();
        let family = path.family;
        let accept_inputs = input_tx.clone();
        session_tasks.spawn(async move {
            accept_incoming_loop(endpoint, family, accept_inputs).await;
        });
    }
    let mut mapping_keepalives = start_mapping_keepalives(&network, &mut session_tasks);

    let semaphore = Arc::new(Semaphore::new(MAX_PENDING_PEERS));
    let mut peer_tasks = JoinSet::new();
    let mut peer_attempts: HashMap<NodeId, (PunchToken, tokio::task::AbortHandle)> = HashMap::new();
    let mut deferred_candidates: HashMap<NodeId, (Vec<Candidate>, PunchToken)> = HashMap::new();
    let mut tunnel_tasks: JoinSet<TunnelTaskResult> = JoinSet::new();
    let selection_workers = Arc::new(Semaphore::new(3));
    let selection_jobs = Arc::new(Semaphore::new(64));
    let continuation_jobs = Arc::new(Semaphore::new(3));
    let mut queue_changes = config.transfer.as_ref().map(|service| service.subscribe());
    let mut peers = PeerRegistry::default();
    let mut connections: HashMap<NodeId, (u64, quinn::Connection)> = HashMap::new();
    let mut reconnect_probes: HashMap<NodeId, VecDeque<ReconnectProbe>> = HashMap::new();
    let mut peer_capabilities: HashMap<NodeId, u64> = HashMap::new();
    let mut peer_connection_updates: HashMap<NodeId, watch::Sender<Option<quinn::Connection>>> =
        HashMap::new();
    let mut tunnel_stop_waiters: HashMap<String, Vec<oneshot::Sender<()>>> = HashMap::new();
    let mut tunnel_rules: HashMap<String, crate::node::core::config::TunnelRule> = config
        .tunnel_rules
        .iter()
        .cloned()
        .map(|rule| (rule.id.clone(), rule))
        .collect();
    let mut waiting_tunnels = HashMap::new();
    let credential_fallback = {
        #[cfg(test)]
        {
            config.test_outgoing_password.is_some()
        }
        #[cfg(not(test))]
        {
            false
        }
    };
    let mut running_tunnels: HashMap<String, RunningTunnel> = HashMap::new();
    let mut restart_after_tunnel_stop = HashSet::new();
    let mut forced_tunnel_errors: HashMap<String, String> = HashMap::new();
    // Candidate sets are bound to the same peer generation as authenticated connections.
    let mut peer_candidates: HashMap<NodeId, (u64, Vec<Candidate>)> = HashMap::new();
    let mut pending_inbound: HashMap<(NodeId, u64), mpsc::Sender<quinn::Incoming>> = HashMap::new();
    let mut queued_lookups: VecDeque<(NodeId, u64)> = VecDeque::new();
    let mut signal: Option<SignalingClient> = None;
    let mut active_relay_server = config.network.relay_server.clone();
    let mut signal_server = config.signal_server;
    let mut signal_tls = config.signal_tls;
    let mut signal_generation = 0u64;
    let mut connect_task: Option<tokio::task::AbortHandle> = None;
    let mut reconnect_sleep: Option<Pin<Box<Sleep>>> = None;
    let mut reconnect_attempt = 0u32;
    let mut session_shutdown = false;
    let mut maintenance = time::interval(Duration::from_secs(1));
    #[cfg(test)]
    let mut test_network_losses = HashSet::new();

    start_signal_connect(
        &identity,
        (&signal_server, signal_tls.as_ref()),
        &network.local_candidates,
        signal_generation,
        input_tx.clone(),
        &mut connect_task,
        &mut session_tasks,
    );

    for rule in config
        .tunnel_rules
        .iter()
        .filter(|rule| rule.enabled && rule.auto_start)
        .cloned()
    {
        start_tunnel_rule(
            rule,
            local_node,
            &connections,
            &peers,
            &credentials,
            credential_fallback,
            &mut waiting_tunnels,
            &peer_capabilities,
            &mut peer_connection_updates,
            &mut running_tunnels,
            &mut tunnel_tasks,
            &command_tx,
            &events,
            &tunnel_policy,
            &forward_streams,
        )
        .await;
    }

    while !session_shutdown {
        enum Wake {
            Command(Option<SessionCommand>),
            Signal(Result<SignalMessage>),
            Input(Option<SessionInput>),
            Retry,
            ReconnectProbe(NodeId, usize, bool),
            Maintenance,
            Discovery(std::result::Result<mdns_sd::ServiceEvent, String>),
            QueueChanged,
            Shutdown,
            EventsClosed,
            Task(Option<std::result::Result<(), tokio::task::JoinError>>),
            TunnelTask(TunnelTaskJoin),
        }

        let wake = tokio::select! {
            command = commands.recv() => Wake::Command(command),
            changed = shutdown.changed() => {
                let _ = changed;
                Wake::Shutdown
            }
            _ = events.sender.closed() => Wake::EventsClosed,
            _ = maintenance.tick() => Wake::Maintenance,
            (peer, index, ready) = std::future::poll_fn(|cx| poll_reconnect_probes(&mut reconnect_probes, cx)) => Wake::ReconnectProbe(peer, index, ready),
            _ = async {
                match queue_changes.as_mut() {
                    Some(changes) => {let _ = changes.changed().await;},
                    None => std::future::pending::<()>().await,
                }
            } => Wake::QueueChanged,
            message = async {
                match signal.as_mut() {
                    Some(client) => client.next_event().await,
                    None => std::future::pending::<Result<SignalMessage>>().await,
                }
            } => Wake::Signal(message),
            input = inputs.recv() => Wake::Input(input),
            message = async {
                match discovery.as_ref() {
                    Some(d) => d.receiver.recv_async().await.map_err(|e| e.to_string()),
                    None => std::future::pending().await,
                }
            } => Wake::Discovery(message),
            _ = async {
                match reconnect_sleep.as_mut() {
                    Some(sleep) => sleep.as_mut().await,
                    None => std::future::pending::<()>().await,
                }
            } => Wake::Retry,
            task = peer_tasks.join_next(), if !peer_tasks.is_empty() => Wake::Task(task),
            task = session_tasks.join_next(), if !session_tasks.is_empty() => Wake::Task(task),
            task = tunnel_tasks.join_next(), if !tunnel_tasks.is_empty() => Wake::TunnelTask(task),
        };

        if let Wake::Discovery(message) = wake {
            match message {
                Ok(event) => nearby.observe(event, std::time::Instant::now()),
                Err(_) => {
                    discovery = None;
                    discovery_state = "unavailable".into();
                }
            }
            let updated = crate::node::core::connectivity::snapshot(
                &network,
                config.direct.enabled,
                discovery_state.clone(),
                &nearby,
            );
            if updated != connectivity {
                connectivity = updated;
                let _ = events
                    .send(SessionEvent::Connectivity(connectivity.clone()))
                    .await;
            }
            continue;
        }
        if matches!(wake, Wake::Maintenance) {
            nearby.expire(std::time::Instant::now());
            let updated = crate::node::core::connectivity::snapshot(
                &network,
                config.direct.enabled,
                discovery_state.clone(),
                &nearby,
            );
            if updated != connectivity {
                connectivity = updated;
                let _ = events
                    .send(SessionEvent::Connectivity(connectivity.clone()))
                    .await;
            }
        }
        let wake = if matches!(wake, Wake::Maintenance) {
            let healthy = connections
                .values()
                .any(|(_, c)| c.close_reason().is_none());
            let reason = if signal_server.trim().is_empty() && !config.direct.enabled {
                None
            } else {
                events.recovery.lock().unwrap().poll(healthy)
            };
            reason.map_or(wake, |reason| {
                Wake::Command(Some(SessionCommand::RefreshNetwork(reason)))
            })
        } else {
            wake
        };
        let wake = match wake {
            Wake::Command(Some(SessionCommand::RetryCandidates {
                peer,
                candidates,
                token,
            })) => Wake::Signal(Ok(SignalMessage::PeerCandidates {
                node_id: peer,
                candidates,
                token,
            })),
            wake => wake,
        };
        if let Wake::Command(Some(command)) = &wake
            && let Some(peer) = command.business_peer()
            && !peer_capabilities.get(&peer).is_some_and(|caps| {
                caps & command.required_capability() == command.required_capability()
            })
        {
            let _ = events
                .send(SessionEvent::Diagnostic("对端未启用此节点业务能力".into()))
                .await;
            if let Some(request) = command.preview_request() {
                let _ = events
                    .send(SessionEvent::DirectoryPreviewFailed(request))
                    .await;
            }
            if matches!(command, SessionCommand::StartSpeed { .. }) {
                let _ = events.send(SessionEvent::SpeedRequestEnded { peer }).await;
            }
            continue;
        }
        // One authorization gate covers outgoing sensitive commands. Local cancellation,
        // settings, and listener startup remain available while a peer is unavailable.
        if let Wake::Command(Some(command)) = &wake
            && let Some(peer) = command.business_peer()
            && (!peers
                .state(peer)
                .is_some_and(PeerLifecycle::outbound_authorized)
                || !connections
                    .get(&peer)
                    .is_some_and(|(_, connection)| connection.close_reason().is_none()))
        {
            let _ = events
                .send(SessionEvent::Diagnostic(
                    "远程访问尚未授权；请先连接设备并完成密码认证".into(),
                ))
                .await;
            if let Some(request) = command.preview_request() {
                let _ = events
                    .send(SessionEvent::DirectoryPreviewFailed(request))
                    .await;
            }
            if matches!(command, SessionCommand::StartSpeed { .. }) {
                let _ = events.send(SessionEvent::SpeedRequestEnded { peer }).await;
            }
            continue;
        }
        match wake {
            #[cfg(test)]
            Wake::Command(Some(SessionCommand::TestNetworkLoss(peer, done))) => {
                if let Some(generation) = peers.generation(peer) {
                    test_network_losses.insert((peer, generation));
                }
                if let Some(service) = config.transfer.clone() {
                    service.network_lost(peer);
                    peer_tasks.spawn(async move {
                        let _ = service.settle_network_loss(peer).await;
                    });
                }
                let _ = done.send(());
            }
            #[cfg(test)]
            Wake::Command(Some(SessionCommand::InspectDeferred(peer, reply))) => {
                let _ = reply.send(deferred_candidates.get(&peer).map(|(_, token)| *token));
            }
            #[cfg(test)]
            Wake::Command(Some(SessionCommand::Inspect(reply))) => {
                let _ = reply.send(connections.clone());
            }
            Wake::Command(Some(SessionCommand::ConnectionUpdates { peer, done })) => {
                if peer_connection_updates.len() >= MAX_PEERS
                    && !peer_connection_updates.contains_key(&peer)
                {
                    peer_connection_updates.retain(|_, sender| sender.receiver_count() > 0);
                }
                if peer == local_node
                    || (peer_connection_updates.len() >= MAX_PEERS
                        && !peer_connection_updates.contains_key(&peer))
                {
                    let _ = done.send(Err("转发对端超出上限或指向本机".into()));
                    continue;
                }
                let authorized = peers
                    .state(peer)
                    .is_some_and(PeerLifecycle::outbound_authorized)
                    && !resource_policy.borrow().for_peer(peer).disabled
                    && peer_capabilities
                        .get(&peer)
                        .is_some_and(|c| c & crate::node::core::protocol::CAP_TCP_TUNNEL != 0);
                let current = connections
                    .get(&peer)
                    .filter(|(_, c)| authorized && c.close_reason().is_none())
                    .map(|(_, c)| c.clone());
                let sender = peer_connection_updates
                    .entry(peer)
                    .or_insert_with(|| watch::channel(None).0);
                sender.send_replace(current);
                let _ = done.send(Ok(sender.subscribe()));
            }
            Wake::Command(Some(SessionCommand::CommitAgentPolicy {
                record,
                path,
                options,
                done,
            })) => {
                // Bind preparation belongs to Agent. This actor is the single
                // authorization publisher and persists before exposing grants.
                if let Err(error) = record.validate() {
                    let _ = done.send(Err(error.to_string()));
                    continue;
                }
                if let Some((peer, generation)) = options.verified {
                    let valid = connections
                        .get(&peer)
                        .is_some_and(|(g, c)| *g == generation && c.close_reason().is_none())
                        && authorizations.get(&peer).is_some_and(|(g, _, rights)| {
                            *g == generation && rights.borrow().inbound.password
                        });
                    if !valid {
                        let _ = done.send(Err("密码证明已失效；请重新连接后再信任设备".into()));
                        continue;
                    }
                }
                if let Some(service) = &config.transfer
                    && service.receive_root_path() != record.settings.receive_root
                    && let Err(error) = crate::node::core::config::validate_receive_directory(
                        &record.settings.receive_root,
                    )
                {
                    let _ = done.send(Err(error.to_string()));
                    continue;
                }
                let outcome = match crate::storage::atomic::write_json(&path, &record) {
                    Ok(outcome) => outcome,
                    Err(error) => {
                        let _ = done.send(Err(error.to_string()));
                        continue;
                    }
                };
                if configured_direct_routes != record.settings.direct.peers {
                    for peer in configured_direct_routes.keys() {
                        direct_routes.remove(peer);
                    }
                    configured_direct_routes = record.settings.direct.peers.clone();
                    // Persisted routes take priority over bounded ephemeral hints.
                    while direct_routes
                        .keys()
                        .filter(|key| !configured_direct_routes.contains_key(*key))
                        .count()
                        + configured_direct_routes.len()
                        > 128
                    {
                        let Some(key) = direct_routes
                            .keys()
                            .find(|key| !configured_direct_routes.contains_key(*key))
                            .cloned()
                        else {
                            break;
                        };
                        direct_routes.remove(&key);
                    }
                    direct_routes.extend(configured_direct_routes.clone());
                }
                let password_changed = remote_verifier != record.settings.remote_auth;
                let previous = resource_policy.borrow().clone();
                let old_trusted = trusted_devices.clone();
                trusted_devices = record.settings.trusted_devices.clone();
                remote_verifier = record.settings.remote_auth.clone();
                auth_context.set_trusted(&trusted_devices);
                resource_policy.send_replace(record.settings.resources.clone());
                allowed_forward_targets.send_replace(record.settings.services.clone());
                let sources = record
                    .settings
                    .trust_sources
                    .iter()
                    .filter_map(|(id, source)| NodeId::from_hex(id).ok().map(|id| (id, *source)))
                    .collect();
                events.control.set_trust_sources(&sources);
                let revoke = connections
                    .keys()
                    .chain(peer_attempts.keys())
                    .copied()
                    .filter(|peer| {
                        let old = previous.for_peer(*peer);
                        let next = record.settings.resources.for_peer(*peer);
                        password_changed
                            || options.restart
                            || next.disabled
                            || (old.files && !next.files)
                            || (old.speed && !next.speed)
                            || (crate::node::core::trusted_devices::contains(&old_trusted, *peer)
                                && !crate::node::core::trusted_devices::contains(
                                    &trusted_devices,
                                    *peer,
                                ))
                    })
                    .collect::<HashSet<_>>();
                if password_changed {
                    credentials.clear();
                    short_queries.clear();
                    credential_retries.clear();
                }
                for peer in revoke {
                    direct_dialing.remove(&peer);
                    credentials.remove(&peer);
                    credential_retries.remove(&peer);
                    deferred_candidates.remove(&peer);
                    peer_candidates.remove(&peer);
                    reconnect_probes.remove(&peer);
                    pending_inbound.retain(|(id, _), _| *id != peer);
                    queued_lookups.retain(|(id, _)| *id != peer);
                    if let Some((_, attempt)) = peer_attempts.remove(&peer) {
                        attempt.abort();
                    }
                    if let Some((_, _, rights)) = authorizations.get(&peer) {
                        rights.send_replace(RemoteAuthorization::default());
                    }
                    if let Some((_, connection)) = connections.get(&peer) {
                        connection.close(3u32.into(), b"node policy revoked");
                    }
                    if let Some(sender) = peer_connection_updates.get(&peer) {
                        sender.send_replace(None);
                    }
                    if let Some(service) = &config.transfer {
                        service.cancel_peer_recovery(peer);
                    }
                }
                if let Some(service) = &config.transfer {
                    // Values have already been validated before persistence.
                    if service.receive_root_path() != record.settings.receive_root {
                        service.set_receive_root(record.settings.receive_root.clone());
                    }
                    service.set_auto_resume(record.settings.auto_resume);
                    let _ = service.set_send_limit(record.settings.send_concurrency);
                    let _ = service.set_file_limits(record.settings.file_limits);
                    let _ = service
                        .receive_space
                        .set_safety_mib(record.settings.receive_safety_mib);
                }
                if options.restart {
                    session_shutdown = true;
                }
                let _ = done.send(Ok(outcome));
            }
            Wake::Command(Some(SessionCommand::ListServices { peer, done })) => {
                if !peer_capabilities
                    .get(&peer)
                    .is_some_and(|caps| caps & crate::node::core::wire::CAP_SERVICE_DIRECTORY != 0)
                {
                    let _ = done.send(Err("对端未协商统一服务目录能力".into()));
                    continue;
                }
                let valid = peers
                    .state(peer)
                    .is_some_and(PeerLifecycle::outbound_authorized)
                    && !resource_policy.borrow().for_peer(peer).disabled;
                let connection = connections
                    .get(&peer)
                    .filter(|(_, connection)| valid && connection.close_reason().is_none())
                    .map(|(_, c)| c.clone());
                let Some(connection) = connection else {
                    let _ = done.send(Err("对端当前没有有效出站授权".into()));
                    continue;
                };
                let Some((_, _, rights)) = authorizations.get(&peer) else {
                    let _ = done.send(Err("会话授权不可用".into()));
                    continue;
                };
                let Ok(permit) = service_queries.clone().try_acquire_owned() else {
                    let _ = done.send(Err("目录查询达到并发上限".into()));
                    continue;
                };
                let live = rights.live();
                peer_tasks.spawn(async move {
                    let _permit = permit;
                    let result = live
                        .guard(false, crate::node::core::services::list(&connection))
                        .await
                        .map_err(|error| error.to_string());
                    let _ = done.send(result);
                });
            }
            Wake::Command(Some(SessionCommand::DisconnectPeer { peer, done })) => {
                requested_peers.remove(&peer);
                direct_dialing.remove(&peer);
                if manually_disconnected.len() >= 128 && !manually_disconnected.contains(&peer) {
                    let _ = done.send(Err("显式断开记录已达上限；请复用现有节点或重启宿主".into()));
                    continue;
                }
                manually_disconnected.insert(peer);
                credentials.remove(&peer);
                credential_retries.remove(&peer);
                deferred_candidates.remove(&peer);
                peer_candidates.remove(&peer);
                reconnect_probes.remove(&peer);
                pending_inbound.retain(|(id, _), _| *id != peer);
                queued_lookups.retain(|(id, _)| *id != peer);
                if let Some((_, attempt)) = peer_attempts.remove(&peer) {
                    attempt.abort();
                }
                if let Some((_, _, rights)) = authorizations.remove(&peer) {
                    rights.send_replace(RemoteAuthorization::default());
                }
                if let Some((_, connection)) = connections.remove(&peer) {
                    connection.close(3u32.into(), b"explicit peer disconnect");
                }
                peer_capabilities.remove(&peer);
                if let Some(update) = peer_connection_updates.get(&peer) {
                    update.send_replace(None);
                }
                for (id, running) in &running_tunnels {
                    if running.rule.peer_node_id == peer.to_hex() {
                        waiting_tunnels.remove(id);
                        restart_after_tunnel_stop.remove(id);
                        running.shutdown.send_replace(true);
                    }
                }
                let result = if let Some(service) = &config.transfer {
                    service.cancel_peer_recovery(peer);
                    service
                        .revoke_authorization(peer, RemoteAuthorization::default())
                        .await
                        .map_err(|e| e.to_string())
                } else {
                    Ok(())
                };
                if let Some(generation) = peers.generation(peer) {
                    peers.transition(peer, generation, PeerLifecycle::Disconnected);
                    emit_peer_state(&events, peer, generation, PeerLifecycle::Disconnected).await;
                }
                let _ = done.send(result);
            }
            Wake::Command(Some(SessionCommand::ResourcePolicy { policy, done })) => {
                // Publish before processing any later queued open or handshake callback.
                resource_policy.send_replace(policy);
                let disabled = connections
                    .keys()
                    .chain(peer_attempts.keys())
                    .copied()
                    .filter(|peer| resource_policy.borrow().for_peer(*peer).disabled)
                    .collect::<HashSet<_>>();
                for peer in disabled {
                    credentials.remove(&peer);
                    credential_retries.remove(&peer);
                    if let Some((_, attempt)) = peer_attempts.remove(&peer) {
                        attempt.abort();
                    }
                    if let Some((_, _, rights)) = authorizations.get(&peer) {
                        rights.send_replace(RemoteAuthorization::default());
                    }
                    if let Some((_, connection)) = connections.get(&peer) {
                        connection.close(3u32.into(), b"peer disabled");
                    }
                    if let Some(service) = &config.transfer {
                        service.cancel_peer_recovery(peer);
                    }
                }
                let _ = done.send(());
            }
            Wake::Command(Some(SessionCommand::ChangeTrustedDevice { change, done })) => {
                let verified_trust = match &change {
                    TrustedDeviceChange::Trust { peer, .. } => Some(*peer),
                    _ => None,
                };
                let candidate: std::result::Result<
                    Vec<crate::node::core::trusted_devices::TrustedDevice>,
                    String,
                > = (|| {
                    let mut devices = trusted_devices.clone();
                    match change {
                        TrustedDeviceChange::Trust {
                            peer,
                            generation,
                            display_name,
                        } => {
                            let valid = peer != local_node
                                && connections.get(&peer).is_some_and(|(g, c)| {
                                    *g == generation && c.close_reason().is_none()
                                })
                                && authorizations.get(&peer).is_some_and(|(g, _, a)| {
                                    *g == generation && {
                                        let a = a.borrow();
                                        a.inbound.password
                                    }
                                });
                            if !valid {
                                return Err("只能明确信任当前已证明知道本机密码的真实设备".into());
                            }
                            if crate::node::core::trusted_devices::contains(&devices, peer) {
                                return Err("设备已经受信任".into());
                            }
                            devices.push(crate::node::core::trusted_devices::TrustedDevice::new(
                                peer,
                                display_name,
                                resolved_short_ids.get(&peer).map(ToString::to_string),
                            ));
                        }
                        TrustedDeviceChange::Rename { peer, display_name } => {
                            let device = devices
                                .iter_mut()
                                .find(|d| d.node_id == peer.to_hex())
                                .ok_or("可信设备不存在")?;
                            device.display_name = display_name;
                            device.updated_at = std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_secs()
                                .max(device.trusted_at);
                        }
                        TrustedDeviceChange::Revoke { peer } => {
                            devices.retain(|d| d.node_id != peer.to_hex())
                        }
                    }
                    crate::node::core::trusted_devices::validate(&devices)
                        .map_err(|e| e.to_string())?;
                    let path = config
                        .config_path
                        .as_ref()
                        .ok_or("配置路径不可用，未修改可信设备")?;
                    crate::node::core::config::DesktopConfig::save_trusted_devices(
                        path,
                        devices.clone(),
                    )
                    .map_err(|e| e.to_string())?;
                    Ok(devices)
                })();
                if let Ok(devices) = &candidate {
                    let revoked = trusted_devices
                        .iter()
                        .filter_map(|old| NodeId::from_hex(&old.node_id).ok())
                        .filter(|peer| {
                            !crate::node::core::trusted_devices::contains(devices, *peer)
                        })
                        .collect::<Vec<_>>();
                    if let Some(peer) = verified_trust {
                        events.control.record_trust_source(
                            peer,
                            crate::node::core::authorization::TrustSource::PasswordVerified,
                        );
                    }
                    for peer in revoked {
                        direct_dialing.remove(&peer);
                        // Strong revocation invalidates password and trust grants alike.
                        credentials.remove(&peer);
                        credential_retries.remove(&peer);
                        if let Some((_, attempt)) = peer_attempts.remove(&peer) {
                            attempt.abort();
                        }
                        if let Some((_, _, rights)) = authorizations.get(&peer) {
                            rights.send_replace(RemoteAuthorization::default());
                        }
                        if let Some((_, connection)) = connections.get(&peer) {
                            connection.close(3u32.into(), b"device trust revoked");
                        }
                        if let Some(service) = &config.transfer {
                            service.cancel_peer_recovery(peer);
                        }
                    }
                    events.control.retain_trust_sources(devices);
                    trusted_devices.clone_from(devices);
                    // Publish revocation synchronously after durable success. No handshake callback can restore it.
                    for (peer, (_, _, rights)) in &authorizations {
                        rights.send_modify(|a| {
                            a.inbound.trusted_device &=
                                crate::node::core::trusted_devices::contains(devices, *peer)
                        });
                    }
                    auth_context.set_trusted(devices);
                    for (peer, (generation, _, rights)) in &authorizations {
                        let authorization = *rights.borrow();
                        if peers.transition(
                            *peer,
                            *generation,
                            PeerLifecycle::Connected(authorization),
                        ) {
                            emit_peer_state(
                                &events,
                                *peer,
                                *generation,
                                PeerLifecycle::Connected(authorization),
                            )
                            .await;
                        }
                    }
                }
                let _ = done.send(candidate);
            }
            Wake::Input(Some(SessionInput::AuthorizationChanged {
                peer,
                generation,
                transport,
            })) => {
                let Some((g, t, rights)) = authorizations.get(&peer) else {
                    continue;
                };
                if *g != generation
                    || *t != transport
                    || !connections.get(&peer).is_some_and(|(g, c)| {
                        *g == generation && c.stable_id() == transport && c.close_reason().is_none()
                    })
                {
                    continue;
                }
                rights.send_if_modified(|a| {
                    let old = *a;
                    a.inbound.trusted_device &= auth_context.trusts(peer);
                    old != *a
                });
                let authorization = *rights.borrow();
                if !peers.transition(peer, generation, PeerLifecycle::Connected(authorization)) {
                    continue;
                }
                emit_peer_state(
                    &events,
                    peer,
                    generation,
                    PeerLifecycle::Connected(authorization),
                )
                .await;
                if let Some(service) = &config.transfer
                    && let Err(error) = service.revoke_authorization(peer, authorization).await
                {
                    let _ = events
                        .send(SessionEvent::Diagnostic(format!(
                            "授权已撤销；任务中断状态保存失败：{error}"
                        )))
                        .await;
                }
                if authorization.outbound_authorized() {
                    if let Some(updates) = peer_connection_updates.get(&peer) {
                        updates.send_replace(
                            connections.get(&peer).map(|(_, c)| c.clone()).filter(|_| {
                                peer_capabilities.get(&peer).is_some_and(|c| {
                                    c & crate::node::core::protocol::CAP_TCP_TUNNEL != 0
                                })
                            }),
                        );
                    }
                    let ready = waiting_tunnels
                        .values()
                        .filter(|r: &&crate::node::core::config::TunnelRule| {
                            r.peer_node_id == peer.to_hex()
                        })
                        .cloned()
                        .collect::<Vec<_>>();
                    for rule in ready {
                        start_tunnel_rule(
                            rule,
                            local_node,
                            &connections,
                            &peers,
                            &credentials,
                            credential_fallback,
                            &mut waiting_tunnels,
                            &peer_capabilities,
                            &mut peer_connection_updates,
                            &mut running_tunnels,
                            &mut tunnel_tasks,
                            &command_tx,
                            &events,
                            &tunnel_policy,
                            &forward_streams,
                        )
                        .await;
                    }
                } else {
                    if let Some(updates) = peer_connection_updates.get(&peer) {
                        updates.send_replace(None);
                    }
                    for (id, running) in &running_tunnels {
                        if running.rule.peer_node_id == peer.to_hex() {
                            running.shutdown.send_replace(true);
                            waiting_tunnels.insert(id.clone(), running.rule.clone());
                            let _ = events
                                .send(SessionEvent::TunnelState {
                                    rule_id: id.clone(),
                                    state: TunnelRuntimeState::WaitingAuthorization,
                                })
                                .await;
                        }
                    }
                }
            }
            Wake::Command(Some(SessionCommand::RetryCandidates { .. })) => {
                unreachable!("candidate retry normalized before dispatch")
            }
            Wake::Discovery(_) => unreachable!("discovery handled above"),
            Wake::Command(None) => {
                session_shutdown = true;
            }
            Wake::Command(Some(SessionCommand::StartSpeed {
                peer,
                direction,
                seconds,
            })) => {
                if let Some(service) = config.transfer.clone() {
                    let live = authorizations[&peer].2.live();
                    let events = events.clone();
                    peer_tasks.spawn(async move {
                        let result = live
                            .guard(false, async {
                                match direction {
                                    crate::node::core::config::SpeedtestDirection::Both => {
                                        match service
                                            .start_speed(
                                                peer,
                                                crate::node::core::protocol::SpeedDirection::Upload,
                                                seconds,
                                            )
                                            .await
                                        {
                                            Ok(upload) => {
                                                let _ = events
                                                    .send(SessionEvent::SpeedPhaseCompleted {
                                                        peer,
                                                        snapshot: upload,
                                                    })
                                                    .await;
                                                let download = service
                                                    .start_speed(
                                                        peer,
                                                        crate::node::core::protocol::SpeedDirection::Download,
                                                        seconds,
                                                    )
                                                    .await;
                                                if let Ok(snapshot) = &download {
                                                    let _ = events
                                                        .send(SessionEvent::SpeedPhaseCompleted {
                                                            peer,
                                                            snapshot: snapshot.clone(),
                                                        })
                                                        .await;
                                                }
                                                download
                                            }
                                            Err(error) => Err(error),
                                        }
                                    }
                                    crate::node::core::config::SpeedtestDirection::Upload
                                    | crate::node::core::config::SpeedtestDirection::Download => {
                                        let wire_direction = match direction {
                                            crate::node::core::config::SpeedtestDirection::Upload => {
                                                crate::node::core::protocol::SpeedDirection::Upload
                                            }
                                            crate::node::core::config::SpeedtestDirection::Download => {
                                                crate::node::core::protocol::SpeedDirection::Download
                                            }
                                            crate::node::core::config::SpeedtestDirection::Both => {
                                                unreachable!()
                                            }
                                        };
                                        let result = service
                                            .start_speed(peer, wire_direction, seconds)
                                            .await;
                                        if let Ok(snapshot) = &result {
                                            let _ = events
                                                .send(SessionEvent::SpeedPhaseCompleted {
                                                    peer,
                                                    snapshot: snapshot.clone(),
                                                })
                                                .await;
                                        }
                                        result
                                    }
                                }
                            })
                            .await;
                        if let Err(error) = result {
                            let _ = events
                                .send(SessionEvent::Diagnostic(error.to_string()))
                                .await;
                        }
                        let _ = events.send(SessionEvent::SpeedRequestEnded { peer }).await;
                    });
                }
            }
            Wake::Command(Some(SessionCommand::CancelSpeed { peer, id })) => {
                if let Some(service) = config.transfer.as_ref()
                    && let Err(error) = service.cancel_speed(peer, &id)
                {
                    let _ = events
                        .send(SessionEvent::Diagnostic(error.to_string()))
                        .await;
                }
            }
            Wake::Command(Some(SessionCommand::PauseTask(id))) => {
                if let Some(service) = &config.transfer
                    && let Err(error) = service.pause_task(id).await
                {
                    let _ = events
                        .send(SessionEvent::Diagnostic(error.to_string()))
                        .await;
                }
            }
            Wake::Command(Some(SessionCommand::CancelDirectoryPreview(id))) => {
                if let Some(service) = &config.transfer {
                    service.cancel_preview(&id);
                }
            }
            Wake::Command(Some(
                command @ (SessionCommand::PreviewDirectory { .. }
                | SessionCommand::ConfirmDirectory { .. }
                | SessionCommand::SendPaths { .. }
                | SessionCommand::SendFile { .. }
                | SessionCommand::SendDirectory { .. }
                | SessionCommand::ResumeTask { .. }),
            )) => {
                let peer = match &command {
                    SessionCommand::PreviewDirectory { peer, .. }
                    | SessionCommand::ConfirmDirectory { peer, .. }
                    | SessionCommand::SendPaths { peer, .. }
                    | SessionCommand::SendFile { peer, .. }
                    | SessionCommand::SendDirectory { peer, .. }
                    | SessionCommand::ResumeTask { peer, .. } => *peer,
                    _ => unreachable!(),
                };
                let preview_request = command.preview_request();
                let Some(service) = config.transfer.clone() else {
                    let _ = events
                        .send(SessionEvent::Diagnostic("任务存储尚未就绪".into()))
                        .await;
                    if let Some(request) = &preview_request {
                        let _ = events
                            .send(SessionEvent::DirectoryPreviewFailed(request.clone()))
                            .await;
                    }
                    continue;
                };
                if peer == local_node {
                    let _ = events
                        .send(SessionEvent::Diagnostic("不能向本机发送任务".into()))
                        .await;
                    if let Some(request) = &preview_request {
                        let _ = events
                            .send(SessionEvent::DirectoryPreviewFailed(request.clone()))
                            .await;
                    }
                    continue;
                }
                let connection = connections
                    .get(&peer)
                    .map(|(_, connection)| connection.clone());
                let job_pool = if matches!(&command, SessionCommand::ResumeTask { .. }) {
                    continuation_jobs.clone()
                } else {
                    selection_jobs.clone()
                };
                let Ok(permit) = job_pool.try_acquire_owned() else {
                    let _ = events
                        .send(SessionEvent::Diagnostic(
                            "文件命令排队已达上限，请稍后重试".into(),
                        ))
                        .await;
                    if let Some(request) = &preview_request {
                        let _ = events
                            .send(SessionEvent::DirectoryPreviewFailed(request.clone()))
                            .await;
                    }
                    continue;
                };
                if let Some(request) = &preview_request
                    && let Err(error) = service.begin_preview(peer, request.clone())
                {
                    let _ = events
                        .send(SessionEvent::DirectoryPreviewFailed(request.clone()))
                        .await;
                    let _ = events
                        .send(SessionEvent::Diagnostic(error.to_string()))
                        .await;
                    continue;
                }
                let live = authorizations[&peer].2.live();
                let selection_workers = selection_workers.clone();
                let events = events.clone();
                peer_tasks.spawn(async move {
                    let _permit = permit;
                    let result = live
                        .guard(false, async {
                            match command {
                                SessionCommand::PreviewDirectory {
                                    source, request, ..
                                } => {
                                    let _worker = selection_workers
                                        .clone()
                                        .acquire_owned()
                                        .await
                                        .map_err(|_| {
                                        crate::node::core::transfer_files::failure("扫描任务已关闭")
                                    })?;
                                    let connection = connection.ok_or_else(|| {
                                        crate::node::core::transfer_files::failure(
                                            "请先连接并授权对端",
                                        )
                                    })?;
                                    let preview = service
                                        .preview_directory_request(
                                            &connection,
                                            peer,
                                            source,
                                            request,
                                        )
                                        .await?;
                                    let _ =
                                        events.send(SessionEvent::DirectoryPreview(preview)).await;
                                    Ok(())
                                }
                                SessionCommand::ConfirmDirectory { id, .. } => {
                                    let connection = connection.ok_or_else(|| {
                                        crate::node::core::transfer_files::failure(
                                            "请先连接并授权对端",
                                        )
                                    })?;
                                    let ids =
                                        service.confirm_directory(&connection, peer, id).await?;
                                    let _ = events
                                        .send(SessionEvent::SelectionQueued {
                                            peer,
                                            count: ids.len(),
                                        })
                                        .await;
                                    Ok(())
                                }
                                SessionCommand::SendPaths { sources, .. } => {
                                    let _worker = selection_workers
                                        .clone()
                                        .acquire_owned()
                                        .await
                                        .map_err(|_| {
                                        crate::node::core::transfer_files::failure("扫描任务已关闭")
                                    })?;
                                    let ids = service.select_paths(peer, sources).await?;
                                    let _ = events
                                        .send(SessionEvent::SelectionQueued {
                                            peer,
                                            count: ids.len(),
                                        })
                                        .await;
                                    Ok(())
                                }
                                SessionCommand::SendFile { source, .. } => {
                                    let _worker = selection_workers
                                        .clone()
                                        .acquire_owned()
                                        .await
                                        .map_err(|_| {
                                        crate::node::core::transfer_files::failure("扫描任务已关闭")
                                    })?;
                                    service.select_file(peer, source).await?;
                                    let _ = events
                                        .send(SessionEvent::SelectionQueued { peer, count: 1 })
                                        .await;
                                    Ok(())
                                }
                                SessionCommand::SendDirectory { source, .. } => {
                                    let _worker = selection_workers
                                        .clone()
                                        .acquire_owned()
                                        .await
                                        .map_err(|_| {
                                        crate::node::core::transfer_files::failure("扫描任务已关闭")
                                    })?;
                                    let ids = service.select_directory(peer, source).await?;
                                    let _ = events
                                        .send(SessionEvent::SelectionQueued {
                                            peer,
                                            count: ids.len(),
                                        })
                                        .await;
                                    Ok(())
                                }
                                SessionCommand::ResumeTask { id, .. } => {
                                    let record = service.task(id.clone()).await?;
                                    if record.direction()
                                        == crate::node::core::task_model::TaskDirection::Send
                                    {
                                        service.enqueue_tasks(peer, vec![id]).await
                                    } else {
                                        let connection = connection.ok_or_else(|| {
                                            crate::node::core::transfer_files::failure(
                                                "请先连接任务绑定的对端",
                                            )
                                        })?;
                                        service.resume(&connection, peer, id).await
                                    }
                                }
                                _ => unreachable!(),
                            }
                        })
                        .await;
                    if let Err(error) = result {
                        if let Some(request) = preview_request {
                            service.cancel_preview(&request);
                            let _ = events
                                .send(SessionEvent::DirectoryPreviewFailed(request))
                                .await;
                        }
                        let _ = events
                            .send(SessionEvent::Diagnostic(error.to_string()))
                            .await;
                    }
                });
            }
            Wake::Command(Some(SessionCommand::ConnectAddress {
                peer,
                addresses,
                password,
                done,
            })) => {
                let result = if !config.direct.enabled {
                    Err("IP直连已禁用".to_owned())
                } else if peer == local_node || resource_policy.borrow().for_peer(peer).disabled {
                    Err("不能连接本机或已禁用的设备".to_owned())
                } else if direct_routes.len() >= 128 && !direct_routes.contains_key(&peer.to_hex())
                {
                    Err("直连设备地址缓存已满".to_owned())
                } else {
                    direct_routes.insert(peer.to_hex(), addresses);
                    let command = password
                        .map_or(SessionCommand::ConnectTrusted(peer), |password| {
                            SessionCommand::ConnectAuthenticated { peer, password }
                        });
                    command_tx
                        .try_send(command)
                        .map_err(|_| "连接队列已满".to_owned())
                };
                let _ = done.send(result);
            }
            Wake::Command(Some(SessionCommand::ConnectAuthenticated { peer, password })) => {
                if requested_peers.len() < MAX_PEERS {
                    requested_peers.insert(peer);
                }
                manually_disconnected.remove(&peer);
                if credentials.len() >= crate::node::core::network_state::MAX_PEERS
                    && !credentials.contains_key(&peer)
                {
                    let _ = events
                        .send(SessionEvent::Diagnostic(
                            "连接凭据数量已达上限，请重启会话后重试".into(),
                        ))
                        .await;
                    continue;
                }
                let same_credential = credentials.get(&peer) == Some(&password);
                #[cfg(test)]
                let same_credential = same_credential
                    || (!credentials.contains_key(&peer)
                        && config.test_outgoing_password.as_ref() == Some(&password));
                let reuse_authorized = same_credential
                    && peers.state(peer).is_some_and(|state| {
                        (state.is_active() && !state.is_connected())
                            || (state.outbound_authorized()
                                && connections
                                    .get(&peer)
                                    .is_some_and(|(_, c)| c.close_reason().is_none()))
                    });
                if !same_credential && let Some(service) = &config.transfer {
                    service.cancel_peer_recovery(peer);
                }
                credentials.insert(peer, password);
                if peers
                    .state(peer)
                    .is_some_and(|state| state.is_active() && !state.is_connected())
                {
                    // A pending lookup will read the new credential when it starts.
                    // A running exchange finishes first, then retries on a fresh transport.
                    // Neither path creates a competing punch token for the same attempt.
                    if !same_credential && peer_attempts.contains_key(&peer) {
                        credential_retries.insert(peer);
                    }
                    continue;
                }
                if !reuse_authorized {
                    if network.relay.is_some() {
                        deferred_candidates.remove(&peer);
                    }
                    if let Some((_, attempt)) = peer_attempts.remove(&peer) {
                        attempt.abort();
                    }
                    reconnect_probes.remove(&peer);
                    // Explicit credentials upgrade rights on a fresh transport; never copy
                    // the previous generation's authorization into a new exchange.
                    if let Some((generation, connection)) = connections.remove(&peer) {
                        if let Some((_, _, rights)) = authorizations.remove(&peer) {
                            rights.send_replace(RemoteAuthorization::default());
                        }
                        retire_peer_listeners(
                            peer,
                            &running_tunnels,
                            &mut waiting_tunnels,
                            &mut restart_after_tunnel_stop,
                        );
                        connection.close(0u32.into(), b"new explicit remote credential");
                        peer_capabilities.remove(&peer);
                        if let Some(service) = config.transfer.as_ref() {
                            service.set_peer_flow_support(peer, false);
                        }
                        if let Some(updates) = peer_connection_updates.get(&peer) {
                            updates.send_replace(None);
                        }
                        peers.transition(peer, generation, PeerLifecycle::Disconnected);
                        emit_peer_state(&events, peer, generation, PeerLifecycle::Disconnected)
                            .await;
                    }
                    if peers.state(peer).is_some_and(PeerLifecycle::is_active)
                        && let Some(generation) = peers.generation(peer)
                    {
                        pending_inbound.remove(&(peer, generation));
                        peers.transition(peer, generation, PeerLifecycle::Disconnected);
                    }
                }
                let _ = command_tx.try_send(SessionCommand::ConnectPeer(peer));
            }
            Wake::Command(Some(SessionCommand::ConnectShort { short_id, password })) => {
                if short_queries.len() >= MAX_PENDING_PEERS || short_queries.contains_key(&short_id)
                {
                    let _ = events
                        .send(SessionEvent::Diagnostic(
                            "短 ID 查询已在进行或达到上限".into(),
                        ))
                        .await;
                    continue;
                }
                if let Some(client) = signal.as_ref()
                    && client.request_short_lookup(short_id).await.is_ok()
                {
                    short_queries.insert(short_id, (password, time::Instant::now()));
                    let _ = events
                        .send(SessionEvent::Diagnostic("正在查询短设备 ID".into()))
                        .await;
                    continue;
                }
                let _ = events
                    .send(SessionEvent::Diagnostic(
                        "信令尚未就绪，无法查询短设备 ID".into(),
                    ))
                    .await;
            }
            Wake::Command(Some(SessionCommand::ConnectTrusted(peer))) => {
                if requested_peers.len() < MAX_PEERS {
                    requested_peers.insert(peer);
                }
                manually_disconnected.remove(&peer);
                let had_password = credentials.remove(&peer).is_some();
                if peers
                    .state(peer)
                    .is_some_and(|state| state.is_active() && !state.is_connected())
                {
                    if had_password && peer_attempts.contains_key(&peer) {
                        credential_retries.insert(peer);
                    }
                    continue;
                }
                let _ = command_tx.try_send(SessionCommand::ConnectPeer(peer));
            }
            Wake::Command(Some(SessionCommand::ConnectPeer(peer))) => {
                if manually_disconnected.contains(&peer)
                    || resource_policy.borrow().for_peer(peer).disabled
                {
                    continue;
                }
                if peer == local_node {
                    let _ = events
                        .send(SessionEvent::Diagnostic("不能连接本机 Node ID".into()))
                        .await;
                    continue;
                }
                match peers.begin_attempt(peer) {
                    BeginPeerAttempt::Started(generation) => {
                        emit_peer_state(&events, peer, generation, PeerLifecycle::PeerPending)
                            .await;
                        emit_lifecycle(&events, NetworkLifecycle::PeerPending { peer }).await;
                        let addresses = if config.direct.enabled {
                            direct_routes
                                .get(&peer.to_hex())
                                .cloned()
                                .unwrap_or_else(|| nearby.addresses(peer))
                        } else {
                            Vec::new()
                        };
                        if !addresses.is_empty() {
                            match direct_slots.clone().try_acquire_owned() {
                                Ok(permit) => {
                                    let task = direct::dial(
                                        &network,
                                        addresses,
                                        peer,
                                        generation,
                                        local_node,
                                        identity.clone(),
                                        input_tx.clone(),
                                        auth_context.clone(),
                                        remote_verifier.clone(),
                                        local_capabilities,
                                        credentials.get(&peer).cloned().or_else(|| {
                                            #[cfg(test)]
                                            {
                                                config.test_outgoing_password.clone()
                                            }
                                            #[cfg(not(test))]
                                            {
                                                None
                                            }
                                        }),
                                        permit,
                                    );
                                    let abort = peer_tasks.spawn(task);
                                    peer_attempts.insert(peer, (PunchToken::random(), abort));
                                    direct_dialing.insert(peer);
                                }
                                Err(_) => {
                                    peers.transition(
                                        peer,
                                        generation,
                                        PeerLifecycle::Failed("直连握手并发已满".into()),
                                    );
                                    emit_peer_state(
                                        &events,
                                        peer,
                                        generation,
                                        PeerLifecycle::Failed("直连握手并发已满".into()),
                                    )
                                    .await;
                                }
                            }
                            continue;
                        }
                        if signal_server.trim().is_empty() {
                            let detail = "未找到直连地址；请选择附近设备或提供完整NodeId与IP:端口"
                                .to_owned();
                            peers.transition(
                                peer,
                                generation,
                                PeerLifecycle::Failed(detail.clone()),
                            );
                            emit_peer_state(
                                &events,
                                peer,
                                generation,
                                PeerLifecycle::Failed(detail),
                            )
                            .await;
                            continue;
                        }
                        if let Some(client) = signal.as_ref() {
                            if client.request_lookup(peer).await.is_err() {
                                queued_lookups.push_back((peer, generation));
                                signal = None;
                                schedule_reconnect(
                                    &events,
                                    &mut reconnect_attempt,
                                    &mut reconnect_sleep,
                                    "信令写入失败".into(),
                                )
                                .await;
                            }
                        } else {
                            queued_lookups.push_back((peer, generation));
                        }
                    }
                    BeginPeerAttempt::AlreadyActive(_) => {
                        // An explicit Connect can discover a restarted peer before Quinn's
                        // old connection idle timeout. Unchanged candidates preserve it.
                        if peers.state(peer).is_some_and(PeerLifecycle::is_connected)
                            && let Some(client) = signal.as_ref()
                        {
                            let message = if client.request_lookup(peer).await.is_ok() {
                                "已请求核对对端地址；现有连接在映射不变时继续使用"
                            } else {
                                "信令查询暂时不可用；现有直连仍保留，请稍后重试连接"
                            };
                            let _ = events.send(SessionEvent::Diagnostic(message.into())).await;
                            continue;
                        }
                        let _ = events
                            .send(SessionEvent::Diagnostic(format!(
                                "已存在对端 {} 的连接请求，忽略重复点击",
                                peer.short()
                            )))
                            .await;
                    }
                    BeginPeerAttempt::AtCapacity => {
                        let _ = events
                            .send(SessionEvent::Diagnostic("待连接对端已达资源上限".into()))
                            .await;
                    }
                }
            }
            Wake::Command(Some(SessionCommand::RefreshNetwork(reason))) => {
                let _ = events.send(SessionEvent::Diagnostic(reason.into())).await;
                // Retire every producer before replacing the input channel: callbacks from
                // old sockets/handshakes must never authenticate into the refreshed network.
                signal_generation = signal_generation.wrapping_add(1);
                signal = None;
                connect_task = None;
                reconnect_sleep = None;
                reconnect_attempt = 0;
                let lost_peers = connections.keys().copied().collect::<Vec<_>>();
                if let Some(service) = &config.transfer {
                    for peer in &lost_peers {
                        service.network_lost(*peer);
                        service.set_peer_flow_support(*peer, false);
                    }
                }
                for (_, (_, _, rights)) in authorizations.drain() {
                    rights.send_replace(RemoteAuthorization::default());
                }
                for (_, (_, connection)) in connections.drain() {
                    connection.close(
                        NETWORK_REFRESH_CLOSE_CODE.into(),
                        b"network candidates refreshed",
                    );
                }
                for update in peer_connection_updates.values() {
                    update.send_replace(None);
                }
                for (peer, generation) in peers.active_peers() {
                    emit_peer_tunnels_starting(peer, &running_tunnels, &events).await;
                    retire_peer_listeners(
                        peer,
                        &running_tunnels,
                        &mut waiting_tunnels,
                        &mut restart_after_tunnel_stop,
                    );
                    peers.transition(peer, generation, PeerLifecycle::Disconnected);
                    emit_peer_state(&events, peer, generation, PeerLifecycle::Disconnected).await;
                }
                peer_tasks.abort_all();
                session_tasks.abort_all();
                while peer_tasks.join_next().await.is_some() {}
                while session_tasks.join_next().await.is_some() {}
                (input_tx, inputs) = mpsc::channel(SESSION_INPUT_CAPACITY);
                if let Some(service) = &config.transfer {
                    for peer in lost_peers {
                        let _ = service.settle_network_loss(peer).await;
                    }
                }
                peer_attempts.clear();
                direct_dialing.clear();
                deferred_candidates.clear();
                reconnect_probes.clear();
                pending_inbound.clear();
                queued_lookups.clear();
                short_queries.clear();
                credential_retries.clear();
                peer_candidates.clear();
                peer_capabilities.clear();
                mapping_keepalives.clear();
                // Keep credentials, trust, explicit routes, manual disconnects and the
                // Agent-owned TCP listeners. They belong to policy, not to UDP sockets.
                discovery = None;
                nearby = crate::node::core::connectivity::NearbyCache::new(local_node);
                network.close();
                let _ = time::timeout(Duration::from_secs(3), network.wait_idle()).await;
                network = DesktopNetwork {
                    paths: vec![],
                    relay: None,
                    local_candidates: vec![],
                    diagnostics: vec![],
                };
                let mut preparation = config.network.clone();
                preparation.relay_server = active_relay_server.clone();
                if signal_server.trim().is_empty() {
                    preparation.stun_servers.clear();
                    preparation.relay_server = None;
                }
                let prepared = tokio::select! {
                    _ = shutdown.changed() => { session_shutdown = true; continue; },
                    prepared = prepare_desktop_network(&preparation) => prepared,
                };
                match prepared {
                    Ok(prepared) => network = prepared,
                    Err(error) => {
                        events.recovery.lock().unwrap().retry_refresh();
                        let _ = events
                            .send(SessionEvent::Diagnostic(format!(
                                "刷新网络失败，将退避重试：{error}"
                            )))
                            .await;
                        emit_lifecycle(
                            &events,
                            NetworkLifecycle::Failed {
                                detail: error.to_string(),
                            },
                        )
                        .await;
                        continue;
                    }
                }
                for path in &network.paths {
                    let endpoint = path.endpoint.clone();
                    let family = path.family;
                    let accept_inputs = input_tx.clone();
                    session_tasks.spawn(async move {
                        accept_incoming_loop(endpoint, family, accept_inputs).await;
                    });
                }
                mapping_keepalives = start_mapping_keepalives(&network, &mut session_tasks);
                if config.direct.enabled && config.direct.lan_discovery {
                    match crate::node::core::connectivity::Discovery::start(local_node, &network) {
                        Ok(d) => {
                            discovery = Some(d);
                            discovery_state = "active".into();
                        }
                        Err(_) => discovery_state = "unavailable".into(),
                    }
                }
                connectivity = crate::node::core::connectivity::snapshot(
                    &network,
                    config.direct.enabled,
                    discovery_state.clone(),
                    &nearby,
                );
                let _ = events
                    .send(SessionEvent::Connectivity(connectivity.clone()))
                    .await;
                let _ = events
                    .send(SessionEvent::NetworkPaths(network.diagnostics.join("; ")))
                    .await;
                let _ = events
                    .send(SessionEvent::NetworkFamilies {
                        ipv4: network.path(AddressFamily::Ipv4).is_some(),
                        ipv6: network.path(AddressFamily::Ipv6).is_some(),
                        relay_configured: network.relay.is_some(),
                    })
                    .await;
                emit_lifecycle(
                    &events,
                    if signal_server.trim().is_empty() {
                        NetworkLifecycle::LocalOnly
                    } else {
                        NetworkLifecycle::ConnectingSignal
                    },
                )
                .await;
                start_signal_connect(
                    &identity,
                    (&signal_server, signal_tls.as_ref()),
                    &network.local_candidates,
                    signal_generation,
                    input_tx.clone(),
                    &mut connect_task,
                    &mut session_tasks,
                );
            }
            Wake::Command(Some(
                command @ (SessionCommand::ReconfigureNetwork { .. }
                | SessionCommand::UpdateRemoteAuth { .. }),
            )) => {
                let mut auth_updated = None;
                match command {
                    SessionCommand::ReconfigureNetwork {
                        server,
                        relay_server,
                        signal_tls: updated_tls,
                    } => {
                        if !server.trim().is_empty()
                            && network.paths.is_empty()
                            && network.relay.is_none()
                        {
                            let mut preparation = config.network.clone();
                            preparation.relay_server = None;
                            match prepare_desktop_network(&preparation).await {
                                Ok(prepared) => network = prepared,
                                Err(error) => {
                                    emit_lifecycle(
                                        &events,
                                        NetworkLifecycle::Failed {
                                            detail: error.to_string(),
                                        },
                                    )
                                    .await;
                                    continue;
                                }
                            }
                            for task in mapping_keepalives.drain(..) {
                                task.abort();
                            }
                            mapping_keepalives =
                                start_mapping_keepalives(&network, &mut session_tasks);
                            for path in &network.paths {
                                let endpoint = path.endpoint.clone();
                                let family = path.family;
                                let inputs = input_tx.clone();
                                session_tasks.spawn(async move {
                                    accept_incoming_loop(endpoint, family, inputs).await;
                                });
                            }
                            let _ = events
                                .send(SessionEvent::NetworkPaths(network.diagnostics.join("; ")))
                                .await;
                            let _ = events
                                .send(SessionEvent::NetworkFamilies {
                                    ipv4: network.path(AddressFamily::Ipv4).is_some(),
                                    ipv6: network.path(AddressFamily::Ipv6).is_some(),
                                    relay_configured: relay_server.is_some(),
                                })
                                .await;
                        }
                        signal_server = server;
                        signal_tls = updated_tls;
                        active_relay_server = relay_server.clone();
                        credentials.clear();
                        if let Some(old) = &network.relay {
                            old.endpoints.close();
                        }
                        network.relay = relay_server.map(|server| {
                            crate::relay::client::Fallback::new(server, config.network.families)
                        });
                    }
                    SessionCommand::UpdateRemoteAuth { verifier, done } => {
                        credentials.clear();
                        remote_verifier = Some(verifier);
                        auth_updated = Some(done);
                    }
                    _ => unreachable!(),
                }
                short_queries.clear();
                credential_retries.clear();
                for (_, (_, _, rights)) in authorizations.drain() {
                    rights.send_replace(RemoteAuthorization::default());
                }
                peer_candidates.clear();
                reconnect_probes.clear();
                let signal_changed = auth_updated.is_none();
                if signal_changed {
                    signal_generation = signal_generation.wrapping_add(1);
                }
                // An explicit server change supersedes this generation's attempts
                // and authenticated peers. An involuntary outage below preserves them.
                for (_, (_, connection)) in connections.drain() {
                    connection.close(0u32.into(), b"desktop network reconfigured");
                }
                peer_attempts.clear();
                direct_dialing.clear();
                deferred_candidates.clear();
                peer_tasks.abort_all();
                while peer_tasks.join_next().await.is_some() {}
                if let Some(service) = &config.transfer {
                    let _ = service.interrupt_all().await;
                }
                pending_inbound.clear();
                queued_lookups.clear();
                peer_capabilities.clear();
                for update in peer_connection_updates.values() {
                    update.send_replace(None);
                }
                for peer in peer_connection_updates.keys() {
                    emit_peer_tunnels_starting(*peer, &running_tunnels, &events).await;
                }
                for (id, running) in &running_tunnels {
                    waiting_tunnels.insert(id.clone(), running.rule.clone());
                    restart_after_tunnel_stop.insert(id.clone());
                    running.shutdown.send_replace(true);
                }
                let tunnel_peers = running_tunnels
                    .values()
                    .filter_map(|running| NodeId::from_hex(&running.rule.peer_node_id).ok())
                    .collect::<HashSet<_>>();
                for peer in tunnel_peers {
                    let _ = command_tx.try_send(SessionCommand::ConnectPeer(peer));
                }
                for (peer, generation) in peers.active_peers() {
                    let state = PeerLifecycle::Failed(
                        if signal_changed {
                            "网络配置已更改，请重新连接"
                        } else {
                            "远程密码已更改，会话授权已撤销，请重新连接"
                        }
                        .into(),
                    );
                    if peers.transition(peer, generation, state.clone()) {
                        emit_peer_state(&events, peer, generation, state).await;
                    }
                }
                if signal_changed {
                    signal = None;
                    reconnect_sleep = None;
                    reconnect_attempt = 0;
                    if let Some(task) = connect_task.take() {
                        task.abort();
                    }
                    let _ = events
                        .send(SessionEvent::SignalSecurity {
                            tls: signal_tls.is_some(),
                        })
                        .await;
                    emit_lifecycle(
                        &events,
                        if signal_server.trim().is_empty() {
                            NetworkLifecycle::Unconfigured
                        } else {
                            NetworkLifecycle::ConnectingSignal
                        },
                    )
                    .await;
                    start_signal_connect(
                        &identity,
                        (&signal_server, signal_tls.as_ref()),
                        &network.local_candidates,
                        signal_generation,
                        input_tx.clone(),
                        &mut connect_task,
                        &mut session_tasks,
                    );
                }
                for rule in waiting_tunnels.values() {
                    if let Ok(peer) = NodeId::from_hex(&rule.peer_node_id) {
                        let _ = command_tx.try_send(SessionCommand::ConnectPeer(peer));
                    }
                }
                if let Some(done) = auth_updated {
                    let _ = done.send(());
                }
            }
            Wake::Command(Some(SessionCommand::StartTunnel(rule_id))) => {
                let Some(rule) = tunnel_rules.get(&rule_id).cloned() else {
                    let _ = events
                        .send(SessionEvent::TunnelState {
                            rule_id,
                            state: TunnelRuntimeState::Error("转发规则不存在".into()),
                        })
                        .await;
                    continue;
                };
                start_tunnel_rule(
                    rule,
                    local_node,
                    &connections,
                    &peers,
                    &credentials,
                    credential_fallback,
                    &mut waiting_tunnels,
                    &peer_capabilities,
                    &mut peer_connection_updates,
                    &mut running_tunnels,
                    &mut tunnel_tasks,
                    &command_tx,
                    &events,
                    &tunnel_policy,
                    &forward_streams,
                )
                .await;
            }
            Wake::Command(Some(SessionCommand::StopTunnel(rule_id))) => {
                waiting_tunnels.remove(&rule_id);
                restart_after_tunnel_stop.remove(&rule_id);
                if let Some(running) = running_tunnels.get(&rule_id) {
                    running.shutdown.send_replace(true);
                } else {
                    let _ = events
                        .send(SessionEvent::TunnelState {
                            rule_id,
                            state: TunnelRuntimeState::Stopped,
                        })
                        .await;
                }
            }
            Wake::Command(Some(SessionCommand::RevokeTunnel {
                rule_id,
                delete,
                done,
            })) => {
                waiting_tunnels.remove(&rule_id);
                if delete {
                    tunnel_rules.remove(&rule_id);
                } else if let Some(rule) = tunnel_rules.get_mut(&rule_id) {
                    rule.enabled = false;
                }
                restart_after_tunnel_stop.remove(&rule_id);
                if let Some(running) = running_tunnels.get(&rule_id) {
                    running.shutdown.send_replace(true);
                    tunnel_stop_waiters.entry(rule_id).or_default().push(done);
                } else {
                    let _ = done.send(());
                }
            }
            Wake::Command(Some(SessionCommand::UpdateTunnelSettings {
                tunnel_rules: next_rules,
            })) => {
                let next_rules = next_rules
                    .into_iter()
                    .map(|rule| (rule.id.clone(), rule))
                    .collect::<HashMap<_, _>>();
                let to_stop = running_tunnels
                    .iter()
                    .filter(|(id, running)| next_rules.get(*id) != Some(&running.rule))
                    .map(|(id, running)| {
                        let restart = next_rules.get(id).is_some_and(|rule| rule.enabled);
                        (id.clone(), restart, running.shutdown.clone())
                    })
                    .collect::<Vec<_>>();
                for (id, restart, shutdown) in to_stop {
                    if restart {
                        restart_after_tunnel_stop.insert(id.clone());
                    } else {
                        restart_after_tunnel_stop.remove(&id);
                    }
                    shutdown.send_replace(true);
                }
                tunnel_rules = next_rules;
                let cancelled = waiting_tunnels
                    .iter()
                    .filter(|(id, waiting)| tunnel_rules.get(*id) != Some(*waiting))
                    .map(|(id, _)| id.clone())
                    .collect::<Vec<_>>();
                for id in cancelled {
                    waiting_tunnels.remove(&id);
                    let _ = events
                        .send(SessionEvent::TunnelState {
                            rule_id: id,
                            state: TunnelRuntimeState::Stopped,
                        })
                        .await;
                }
                let auto_start = tunnel_rules
                    .values()
                    .filter(|rule| rule.enabled && rule.auto_start)
                    .filter(|rule| !running_tunnels.contains_key(&rule.id))
                    .filter(|rule| !restart_after_tunnel_stop.contains(&rule.id))
                    .cloned()
                    .collect::<Vec<_>>();
                for rule in auto_start {
                    start_tunnel_rule(
                        rule,
                        local_node,
                        &connections,
                        &peers,
                        &credentials,
                        credential_fallback,
                        &mut waiting_tunnels,
                        &peer_capabilities,
                        &mut peer_connection_updates,
                        &mut running_tunnels,
                        &mut tunnel_tasks,
                        &command_tx,
                        &events,
                        &tunnel_policy,
                        &forward_streams,
                    )
                    .await;
                }
            }
            Wake::Signal(Ok(message)) => match message {
                SignalMessage::Pong => {
                    let _ = events
                        .send(SessionEvent::Diagnostic("信令心跳已确认".into()))
                        .await;
                }
                SignalMessage::PeerPending { node_id } => {
                    if peers.state(node_id) == Some(&PeerLifecycle::PeerPending) {
                        emit_peer_state(
                            &events,
                            node_id,
                            peers.generation(node_id).unwrap_or_default(),
                            PeerLifecycle::PeerPending,
                        )
                        .await;
                        emit_lifecycle(&events, NetworkLifecycle::PeerPending { peer: node_id })
                            .await;
                    }
                }
                SignalMessage::PeerCandidates {
                    node_id,
                    candidates,
                    token,
                } => {
                    if manually_disconnected.contains(&node_id)
                        || resource_policy.borrow().for_peer(node_id).disabled
                    {
                        continue;
                    }
                    if peers
                        .state(node_id)
                        .is_some_and(|state| state.is_active() && !state.is_connected())
                        && let Some((current_token, _)) = peer_attempts.get(&node_id)
                    {
                        // A new lookup can arrive before the old auth failure callback.
                        // Retain its authenticated signaling candidates rather than losing
                        // the only matching punch token or cancelling an active transport.
                        if *current_token != token {
                            deferred_candidates.insert(node_id, (candidates, token));
                        }
                        continue;
                    }
                    let canonical = canonical_candidates(&candidates);
                    if network.relay.is_some()
                        && peers
                            .state(node_id)
                            .is_some_and(PeerLifecycle::is_connected)
                    {
                        // A real pairing may precede the old transport's close callback.
                        // Direct probes cannot confirm an unchanged blackhole candidate on
                        // a Relay path. Preserve the offer without replacing any live winner;
                        // only actual closure may consume it for a fresh generation.
                        deferred_candidates.insert(node_id, (candidates.clone(), token));
                    }
                    if peers
                        .state(node_id)
                        .is_some_and(PeerLifecycle::is_connected)
                        && peer_candidates
                            .get(&node_id)
                            .is_some_and(|(generation, old)| {
                                peers.is_current(node_id, *generation) && *old == canonical
                            })
                    {
                        let _ = events
                            .send(SessionEvent::Diagnostic(
                                "地址核对完毕，现有认证连接继续使用".into(),
                            ))
                            .await;
                        if candidates.len() <= MAX_CANDIDATES
                            && !network.reachable_candidates(&candidates).is_empty()
                            && connections
                                .get(&node_id)
                                .is_some_and(|(_, c)| c.close_reason().is_none())
                        {
                            let generation = peers.generation(node_id).unwrap();
                            let watches = reconnect_probes.entry(node_id).or_default();
                            if watches.len() >= MAX_RECONNECT_PROBES_PER_PEER {
                                watches.pop_front();
                            }
                            let receivers = network
                                .paths
                                .iter()
                                .filter(|path| !path.reachable_candidates(&candidates).is_empty())
                                .filter_map(|path| {
                                    path.punch_socket
                                        .register_peer_probe(&token, node_id, generation)
                                        .ok()
                                })
                                .collect::<Vec<_>>();
                            if !receivers.is_empty() {
                                watches.push_back(ReconnectProbe {
                                    generation,
                                    candidates,
                                    token,
                                    receivers,
                                    deadline: Box::pin(time::sleep(
                                        PEER_PUNCH_CONFIG.interval * PEER_PUNCH_CONFIG.attempts,
                                    )),
                                });
                            }
                            continue;
                        }
                    }
                    if peers
                        .state(node_id)
                        .is_some_and(PeerLifecycle::is_connected)
                        && (connections
                            .get(&node_id)
                            .is_some_and(|(_, connection)| connection.close_reason().is_some())
                            || peer_candidates
                                .get(&node_id)
                                .is_some_and(|(generation, old)| {
                                    peers.is_current(node_id, *generation) && *old != canonical
                                }))
                        && !network.reachable_candidates(&candidates).is_empty()
                    {
                        // Changed mapping or a closed transport whose callback is still queued.
                        // Retire only this generation; new Punch/QUIC/identity/capability
                        // checks remain mandatory. Old closure callbacks are fenced.
                        if let Some((generation, connection)) = connections.remove(&node_id) {
                            if let Some(service) = config.transfer.clone() {
                                service.network_lost(node_id);
                                peer_tasks.spawn(async move {
                                    let _ = service.settle_network_loss(node_id).await;
                                });
                            }
                            if let Some((_, _, rights)) = authorizations.remove(&node_id) {
                                rights.send_replace(RemoteAuthorization::default());
                            }
                            retire_peer_listeners(
                                node_id,
                                &running_tunnels,
                                &mut waiting_tunnels,
                                &mut restart_after_tunnel_stop,
                            );
                            connection.close(0u32.into(), b"peer mapping changed");
                            peer_capabilities.remove(&node_id);
                            if let Some(service) = config.transfer.as_ref() {
                                service.set_peer_flow_support(node_id, false);
                            }
                            if let Some(updates) = peer_connection_updates.get(&node_id) {
                                updates.send_replace(None);
                            }
                            emit_peer_tunnels_starting(node_id, &running_tunnels, &events).await;
                            if peers.transition(node_id, generation, PeerLifecycle::Disconnected) {
                                emit_peer_state(
                                    &events,
                                    node_id,
                                    generation,
                                    PeerLifecycle::Disconnected,
                                )
                                .await;
                            }
                        }
                        peer_candidates.remove(&node_id);
                        reconnect_probes.remove(&node_id);
                    }
                    if let Some((generation, attempt)) = start_peer_attempt(
                        node_id,
                        candidates,
                        token,
                        local_node,
                        identity.clone(),
                        &network,
                        &events,
                        &input_tx,
                        &semaphore,
                        &mut peers,
                        &mut pending_inbound,
                        &mut peer_tasks,
                        auth_context.clone(),
                        remote_verifier.clone(),
                        AttemptOptions {
                            capabilities: local_capabilities,
                        },
                        credentials.get(&node_id).cloned().or_else(|| {
                            #[cfg(test)]
                            {
                                config.test_outgoing_password.clone()
                            }
                            #[cfg(not(test))]
                            {
                                None
                            }
                        }),
                    )
                    .await
                    {
                        peer_attempts.insert(node_id, (token, attempt));
                        peer_candidates.insert(node_id, (generation, canonical));
                    }
                }
                SignalMessage::ShortResolved { short_id, node_id } => {
                    if let Some((password, _)) = short_queries.remove(&short_id) {
                        let _ = events
                            .send(SessionEvent::ShortIdResolved {
                                short_id,
                                peer: node_id,
                            })
                            .await;
                        if let Some(peer) = node_id {
                            if resolved_short_ids.len()
                                < crate::node::core::network_state::MAX_PEERS
                                || resolved_short_ids.contains_key(&peer)
                            {
                                resolved_short_ids.insert(peer, short_id);
                            }
                            if peer != local_node
                                && (credentials.len() < crate::node::core::network_state::MAX_PEERS
                                    || credentials.contains_key(&peer))
                            {
                                let _ = command_tx.try_send(match password {
                                    Some(password) => {
                                        SessionCommand::ConnectAuthenticated { peer, password }
                                    }
                                    None => SessionCommand::ConnectTrusted(peer),
                                });
                            }
                        } else {
                            let _ = events
                                .send(SessionEvent::Diagnostic(
                                    "短 ID 无法解析或查询暂时受限".into(),
                                ))
                                .await;
                        }
                    }
                }
                SignalMessage::Error { reason } => {
                    let detail = format!("信令查询失败：{reason}");
                    for (peer, generation) in peers.pending_peers() {
                        if peers.transition(peer, generation, PeerLifecycle::Failed(detail.clone()))
                        {
                            fail_peer_tunnels(
                                peer,
                                &detail,
                                &running_tunnels,
                                &mut forced_tunnel_errors,
                            );
                            emit_peer_state(
                                &events,
                                peer,
                                generation,
                                PeerLifecycle::Failed(detail.clone()),
                            )
                            .await;
                        }
                    }
                    queued_lookups.clear();
                    let _ = events
                        .send(SessionEvent::Diagnostic(format!(
                            "信令服务拒绝请求：{reason}"
                        )))
                        .await;
                }
                other => {
                    debug!(
                        kind = other.kind(),
                        "桌面 session 忽略注册阶段以外的信令消息"
                    );
                }
            },
            Wake::Signal(Err(error)) => {
                warn!(%error, "桌面信令连接断开；保留已有认证 P2P 连接");
                signal = None;
                enqueue_pending_lookups(&peers, &mut queued_lookups);
                schedule_reconnect(
                    &events,
                    &mut reconnect_attempt,
                    &mut reconnect_sleep,
                    error.to_string(),
                )
                .await;
            }
            Wake::Input(Some(SessionInput::SignalConnected { generation, result })) => {
                if !is_current_signal_generation(signal_generation, generation) {
                    drop(result);
                    continue;
                }
                connect_task = None;
                match result {
                    Ok(client) => {
                        let registered_node = client.node_id();
                        if let Some(short_id) = client.short_id() {
                            let _ = events.send(SessionEvent::ShortIdRegistered(short_id)).await;
                        }
                        signal = Some(client);
                        reconnect_attempt = 0;
                        reconnect_sleep = None;
                        let _ = events
                            .send(SessionEvent::SignalIdentityRegistered(registered_node))
                            .await;
                        emit_lifecycle(&events, NetworkLifecycle::SignalOnline).await;
                        while let Some((peer, generation)) = queued_lookups.pop_front() {
                            if !peers.is_current(peer, generation)
                                || peers.state(peer) != Some(&PeerLifecycle::PeerPending)
                            {
                                continue;
                            }
                            let Some(client) = signal.as_ref() else {
                                queued_lookups.push_front((peer, generation));
                                break;
                            };
                            if let Err(error) = client.request_lookup(peer).await {
                                warn!(%error, "发送待处理 lookup 失败");
                                queued_lookups.push_front((peer, generation));
                                signal = None;
                                schedule_reconnect(
                                    &events,
                                    &mut reconnect_attempt,
                                    &mut reconnect_sleep,
                                    error.to_string(),
                                )
                                .await;
                                break;
                            }
                        }
                    }
                    Err(error) => {
                        schedule_reconnect(
                            &events,
                            &mut reconnect_attempt,
                            &mut reconnect_sleep,
                            error.to_string(),
                        )
                        .await;
                    }
                }
            }
            Wake::Input(Some(SessionInput::Incoming(family, incoming))) => {
                let incoming = *incoming;
                let remote = incoming.remote_address();
                let route = network
                    .path(family)
                    .and_then(|path| path.punch_socket.claim_authorized_peer(remote))
                    .or_else(|| pending_ipv6_route(remote, local_node, &peers, &peer_candidates));
                if let Some((peer, generation)) = route
                    && peers.is_current(peer, generation)
                    && !should_initiate_quic(local_node, peer)
                    && let Some(waiting) = pending_inbound.get(&(peer, generation))
                {
                    if let Err(error) = waiting.try_send(incoming) {
                        error.into_inner().refuse();
                    }
                    continue;
                }
                if !config.direct.enabled {
                    incoming.refuse();
                    continue;
                }
                // Unknown public sources must first prove return routability using
                // Quinn's stateless Retry; no identity worker is allocated before it.
                if !incoming.remote_address_validated() {
                    let _ = incoming.retry();
                    continue;
                }
                if let Ok(permit) = direct_slots.clone().try_acquire_owned() {
                    let identity = identity.clone();
                    let inputs = input_tx.clone();
                    peer_tasks.spawn(async move {
                        direct::identify(incoming, identity, inputs, permit).await;
                    });
                } else {
                    incoming.refuse();
                }
            }
            Wake::Input(Some(SessionInput::DirectIdentified {
                peer,
                prepared,
                permit,
            })) => {
                if peer == local_node
                    || manually_disconnected.contains(&peer)
                    || resource_policy.borrow().for_peer(peer).disabled
                    || peers.state(peer).is_some_and(PeerLifecycle::is_connected)
                    || (direct_dialing.contains(&peer) && should_initiate_quic(local_node, peer))
                {
                    continue;
                }
                if let Some((_, attempt)) = peer_attempts.remove(&peer) {
                    attempt.abort();
                }
                direct_dialing.remove(&peer);
                if let Some(generation) = peers.generation(peer) {
                    peers.transition(peer, generation, PeerLifecycle::Disconnected);
                    pending_inbound.remove(&(peer, generation));
                }
                let BeginPeerAttempt::Started(generation) = peers.begin_attempt(peer) else {
                    continue;
                };
                queued_lookups.retain(|(id, _)| *id != peer);
                emit_peer_state(&events, peer, generation, PeerLifecycle::Authenticating).await;
                let task = direct::accept(
                    prepared,
                    peer,
                    generation,
                    local_node,
                    input_tx.clone(),
                    auth_context.clone(),
                    remote_verifier.clone(),
                    local_capabilities,
                    credentials.get(&peer).cloned(),
                    permit,
                );
                let abort = peer_tasks.spawn(task);
                peer_attempts.insert(peer, (PunchToken::random(), abort));
            }
            Wake::Input(Some(SessionInput::PeerPath {
                peer,
                generation,
                detail,
            })) => {
                if peers.is_current(peer, generation)
                    && !peers.state(peer).is_some_and(PeerLifecycle::is_connected)
                {
                    let _ = events
                        .send(SessionEvent::PeerPath {
                            peer,
                            generation,
                            detail,
                        })
                        .await;
                }
            }
            Wake::Input(Some(SessionInput::PeerProgress {
                peer,
                generation,
                state,
            })) => {
                if peers.transition(peer, generation, state.clone()) {
                    emit_peer_state(&events, peer, generation, state.clone()).await;
                    if state == PeerLifecycle::Authenticating {
                        emit_lifecycle(&events, NetworkLifecycle::Authenticating { peer }).await;
                    }
                }
            }
            Wake::Input(Some(SessionInput::PeerConnected {
                peer,
                generation,
                capabilities,
                mut authorization,
                control,
                connection,
            })) => {
                pending_inbound.remove(&(peer, generation));
                if manually_disconnected.contains(&peer)
                    || resource_policy.borrow().for_peer(peer).disabled
                {
                    connection.close(3u32.into(), b"peer disabled or explicitly disconnected");
                    continue;
                }
                if !transport_can_publish(&peers, peer, generation, &connection) {
                    continue;
                }
                let _ = events
                    .send(SessionEvent::PeerTransport {
                        peer,
                        generation,
                        relay: network
                            .relay
                            .as_ref()
                            .is_some_and(|r| r.endpoints.is_relay(&connection)),
                        family: AddressFamily::of(connection.remote_address()),
                    })
                    .await;
                let _ = events
                    .send(SessionEvent::PeerPath {
                        peer,
                        generation,
                        detail: format!(
                            "对端 {} 已认证 transport：{}",
                            peer.short(),
                            network.transport_label(&connection)
                        ),
                    })
                    .await;
                peer_attempts.remove(&peer);
                direct_dialing.remove(&peer);
                if credential_retries.remove(&peer) {
                    connection.close(
                        0u32.into(),
                        b"new explicit credential after pending attempt",
                    );
                    peers.transition(peer, generation, PeerLifecycle::Disconnected);
                    let retry = deferred_candidates.remove(&peer).map_or(
                        SessionCommand::ConnectPeer(peer),
                        |(candidates, token)| SessionCommand::RetryCandidates {
                            peer,
                            candidates,
                            token,
                        },
                    );
                    let _ = command_tx.try_send(retry);
                    continue;
                }
                if let Some((candidates, token)) = deferred_candidates.remove(&peer) {
                    let _ = command_tx.try_send(SessionCommand::RetryCandidates {
                        peer,
                        candidates,
                        token,
                    });
                }
                authorization.inbound.trusted_device &= auth_context.trusts(peer);
                let rights =
                    crate::node::core::remote_auth::AuthorizationPublisher::new(authorization);
                let live = rights.subscribe();
                let transport = connection.stable_id();
                if let Some((_, _, old)) =
                    authorizations.insert(peer, (generation, transport, rights.clone()))
                {
                    old.send_replace(RemoteAuthorization::default());
                }
                let control_rights = rights.clone();
                peer_tasks.spawn(async move {
                    (*control).run(control_rights).await;
                });
                let changed_inputs = input_tx.clone();
                let mut changed = live.clone();
                let changed_connection = connection.clone();
                peer_tasks.spawn(async move {
                    loop {
                        tokio::select! {
                            _ = changed_connection.closed() => break,
                            result = changed.changed() => {
                                if result.is_err() { break; }
                                if changed_inputs.send(SessionInput::AuthorizationChanged { peer, generation, transport }).await.is_err() { break; }
                            }
                        }
                    }
                });
                let live = rights.live();
                if !peers.transition(peer, generation, PeerLifecycle::Connected(authorization)) {
                    connection.close(0u32.into(), b"stale peer generation");
                    continue;
                }
                if let Some(short) = resolved_short_ids.get(&peer) {
                    let mut updated = trusted_devices.clone();
                    if let Some(device) = updated.iter_mut().find(|d| {
                        d.node_id == peer.to_hex()
                            && d.last_short_id.as_deref() != Some(short.to_string().as_str())
                    }) {
                        device.last_short_id = Some(short.to_string());
                        device.updated_at = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs()
                            .max(device.trusted_at);
                        if let Some(path) = &config.config_path {
                            match crate::node::core::config::DesktopConfig::save_trusted_devices(
                                path,
                                updated.clone(),
                            ) {
                                Ok(()) => {
                                    trusted_devices = updated;
                                    let _ = events
                                        .send(SessionEvent::TrustedDevicesChanged(
                                            trusted_devices.clone(),
                                        ))
                                        .await;
                                }
                                Err(error) => {
                                    let _ = events
                                        .send(SessionEvent::Diagnostic(format!(
                                            "可信设备 Short ID 备注未保存：{error}"
                                        )))
                                        .await;
                                }
                            }
                        }
                    }
                }
                if capabilities & crate::node::core::protocol::CAP_TRUSTED_DEVICE_AUTH == 0 {
                    let _ = events
                        .send(SessionEvent::Diagnostic(
                            "对端不支持可信设备授权；本次仅使用密码认证".into(),
                        ))
                        .await;
                }
                reconnect_probes.remove(&peer);
                connections.insert(peer, (generation, connection.clone()));
                peer_capabilities.insert(peer, capabilities);
                if let Some(service) = config.transfer.as_ref() {
                    service.set_peer_incremental_support(
                        peer,
                        capabilities & crate::node::core::protocol::CAP_INCREMENTAL_DIRECTORY != 0,
                    );
                    service.set_peer_flow_support(
                        peer,
                        capabilities & crate::node::core::protocol::CAP_FILE_FLOW != 0,
                    );
                }
                if capabilities & crate::node::core::protocol::CAP_AUTO_RESUME == 0
                    && let Some(service) = &config.transfer
                    && service.cancel_peer_recovery(peer)
                {
                    let _ = events
                        .send(SessionEvent::Diagnostic(
                            "对端不支持自动恢复，断线任务请手动继续".into(),
                        ))
                        .await;
                }
                if capabilities & crate::node::core::protocol::CAP_TCP_TUNNEL == 0 {
                    let incompatible_rules = running_tunnels
                        .iter()
                        .filter(|(_, running)| running.rule.peer_node_id == peer.to_hex())
                        .map(|(id, running)| (id.clone(), running.shutdown.clone()))
                        .collect::<Vec<_>>();
                    for (rule_id, stop) in incompatible_rules {
                        let detail =
                            format!("对端设备 {} 不支持 TCP 隧道，请升级桌面端", peer.short());
                        forced_tunnel_errors.insert(rule_id.clone(), detail.clone());
                        stop.send_replace(true);
                        let _ = events
                            .send(SessionEvent::TunnelError { rule_id, detail })
                            .await;
                    }
                }
                peer_connection_updates
                    .entry(peer)
                    .or_insert_with(|| watch::channel(None).0)
                    .send_replace(
                        (authorization.outbound_authorized()
                            && capabilities & crate::node::core::protocol::CAP_TCP_TUNNEL != 0)
                            .then(|| connection.clone()),
                    );
                emit_peer_state(
                    &events,
                    peer,
                    generation,
                    PeerLifecycle::Connected(authorization),
                )
                .await;
                emit_lifecycle(&events, NetworkLifecycle::Connected { peer }).await;
                if authorization.outbound_authorized() {
                    let ready = waiting_tunnels
                        .keys()
                        .filter_map(|id| tunnel_rules.get(id))
                        .filter(|r| {
                            r.enabled && NodeId::from_hex(&r.peer_node_id).ok() == Some(peer)
                        })
                        .cloned()
                        .collect::<Vec<_>>();
                    for rule in ready {
                        start_tunnel_rule(
                            rule,
                            local_node,
                            &connections,
                            &peers,
                            &credentials,
                            credential_fallback,
                            &mut waiting_tunnels,
                            &peer_capabilities,
                            &mut peer_connection_updates,
                            &mut running_tunnels,
                            &mut tunnel_tasks,
                            &command_tx,
                            &events,
                            &tunnel_policy,
                            &forward_streams,
                        )
                        .await;
                    }
                }
                let transfer = config.transfer.clone();
                let business_connection = connection.clone();
                let allowed_targets = allowed_forward_targets.subscribe();
                let resources = resource_policy.subscribe();
                let business_events = events.clone();
                let service_tcp_streams = forward_streams.clone();
                peer_tasks.spawn(async move {
                    let (_dispatcher, incoming) = crate::node::core::dispatch::Dispatcher::start(
                        business_connection.clone(),
                        capabilities,
                    );
                    let crate::node::core::dispatch::Incoming {
                        files,
                        services,
                        speed,
                    } = incoming;
                    if let Some(service) = transfer {
                        let inlets = crate::node::core::transfer::FileInlets::new(
                            files,
                            speed,
                            resources.clone(),
                        );
                        let (file_result, service_result) = tokio::join!(
                            service.serve_dispatched(
                                business_connection,
                                peer,
                                local_node,
                                live.clone(),
                                inlets
                            ),
                            crate::node::core::services::serve(
                                peer,
                                live,
                                allowed_targets,
                                resources,
                                services,
                                service_tcp_streams
                            ),
                        );
                        if file_result.is_err() || service_result.is_err() {
                            let _ = business_events
                                .send(SessionEvent::Diagnostic(
                                    "节点业务会话已结束；任务状态以持久记录为准".into(),
                                ))
                                .await;
                        }
                    } else {
                        drop(files);
                        drop(speed);
                        let _ = crate::node::core::services::serve(
                            peer,
                            live,
                            allowed_targets,
                            resources,
                            services,
                            service_tcp_streams,
                        )
                        .await;
                    }
                });
                let closed_inputs = input_tx.clone();
                peer_tasks.spawn(async move {
                    let reason = connection.closed().await;
                    let network_interruption = recoverable_transport_close(&reason);
                    let detail = reason.to_string();
                    let _ = closed_inputs
                        .send(SessionInput::PeerClosed {
                            peer,
                            generation,
                            detail,
                            network_interruption,
                        })
                        .await;
                });
            }
            Wake::Input(Some(SessionInput::PeerFailed {
                peer,
                generation,
                detail,
            })) => {
                if peers.is_current(peer, generation) {
                    direct_dialing.remove(&peer);
                }
                pending_inbound.remove(&(peer, generation));
                if peers.is_current(peer, generation) {
                    peer_attempts.remove(&peer);
                    let candidate = deferred_candidates.remove(&peer);
                    let credential_retry = credential_retries.remove(&peer);
                    if candidate.is_some() || credential_retry {
                        peers.transition(peer, generation, PeerLifecycle::Failed(detail));
                        let retry = candidate.map_or(
                            SessionCommand::ConnectPeer(peer),
                            |(candidates, token)| SessionCommand::RetryCandidates {
                                peer,
                                candidates,
                                token,
                            },
                        );
                        let _ = command_tx.try_send(retry);
                        continue;
                    }
                }
                if peers.transition(peer, generation, PeerLifecycle::Failed(detail.clone())) {
                    peer_candidates.remove(&peer);
                    fail_peer_tunnels(peer, &detail, &running_tunnels, &mut forced_tunnel_errors);
                    emit_peer_state(
                        &events,
                        peer,
                        generation,
                        PeerLifecycle::Failed(detail.clone()),
                    )
                    .await;
                }
            }
            Wake::Input(Some(SessionInput::PeerClosed {
                peer,
                generation,
                detail,
                network_interruption,
            })) => {
                #[cfg(test)]
                let network_interruption =
                    network_interruption || test_network_losses.remove(&(peer, generation));
                if peers.is_current(peer, generation)
                    && connections
                        .get(&peer)
                        .is_some_and(|(current, _)| *current == generation)
                {
                    if network_interruption && let Some(service) = &config.transfer {
                        service.network_lost(peer);
                    }
                    connections.remove(&peer);
                    if let Some((_, _, rights)) = authorizations.remove(&peer) {
                        rights.send_replace(RemoteAuthorization::default());
                    }
                    if !network_interruption && let Some(service) = &config.transfer {
                        let _ = service
                            .revoke_authorization(peer, RemoteAuthorization::default())
                            .await;
                    }
                    if network_interruption && let Some(service) = config.transfer.clone() {
                        peer_tasks.spawn(async move {
                            let _ = service.settle_network_loss(peer).await;
                        });
                    }
                    peer_capabilities.remove(&peer);
                    if let Some(service) = config.transfer.as_ref() {
                        service.set_peer_flow_support(peer, false);
                    }
                    if let Some(updates) = peer_connection_updates.get(&peer) {
                        updates.send_replace(None);
                    }
                    emit_peer_tunnels_starting(peer, &running_tunnels, &events).await;
                    for (id, running) in &running_tunnels {
                        if running.rule.peer_node_id == peer.to_hex() {
                            waiting_tunnels.insert(id.clone(), running.rule.clone());
                            restart_after_tunnel_stop.insert(id.clone());
                            running.shutdown.send_replace(true);
                        }
                    }
                    let affected_rules = running_tunnels
                        .values()
                        .filter(|running| running.rule.peer_node_id == peer.to_hex())
                        .map(|running| running.rule.id.clone())
                        .collect::<Vec<_>>();
                    for rule_id in &affected_rules {
                        let _ = events
                            .send(SessionEvent::TunnelError {
                                rule_id: rule_id.clone(),
                                detail: format!("对端 {} 已断开：{detail}", peer.short()),
                            })
                            .await;
                    }
                    if !affected_rules.is_empty() {
                        let _ = command_tx.try_send(SessionCommand::ConnectPeer(peer));
                    }
                    peer_candidates.remove(&peer);
                    let relay_retry = if network.relay.is_some() {
                        reconnect_probes.remove(&peer); // retire old token/source routes first
                        deferred_candidates.remove(&peer)
                    } else {
                        None
                    };
                    if peers.transition(peer, generation, PeerLifecycle::Disconnected) {
                        emit_peer_state(&events, peer, generation, PeerLifecycle::Disconnected)
                            .await;
                        emit_lifecycle(
                            &events,
                            NetworkLifecycle::Disconnected {
                                peer: Some(peer),
                                detail,
                            },
                        )
                        .await;
                    }
                    if let Some((candidates, token)) = relay_retry {
                        let _ = command_tx.try_send(SessionCommand::RetryCandidates {
                            peer,
                            candidates,
                            token,
                        });
                    }
                }
            }
            Wake::ReconnectProbe(peer, index, ready) => {
                let Some(probe) = reconnect_probes
                    .get_mut(&peer)
                    .and_then(|w| w.remove(index))
                else {
                    continue;
                };
                let ReconnectProbe {
                    generation,
                    candidates,
                    token,
                    receivers,
                    ..
                } = probe;
                drop(receivers); // Synchronously revoke the old generation's source/token route.
                if !ready
                    || !peers.is_current(peer, generation)
                    || !matches!(
                        peers.state(peer),
                        Some(PeerLifecycle::Connected(_) | PeerLifecycle::Disconnected)
                    )
                {
                    continue;
                }
                reconnect_probes.remove(&peer);
                if let Some((_, old)) = connections.remove(&peer) {
                    if let Some(service) = config.transfer.clone() {
                        service.network_lost(peer);
                        peer_tasks.spawn(async move {
                            let _ = service.settle_network_loss(peer).await;
                        });
                    }
                    if let Some((_, _, rights)) = authorizations.remove(&peer) {
                        rights.send_replace(RemoteAuthorization::default());
                    }
                    retire_peer_listeners(
                        peer,
                        &running_tunnels,
                        &mut waiting_tunnels,
                        &mut restart_after_tunnel_stop,
                    );
                    old.close(0u32.into(), b"peer requested fresh authenticated transport");
                    peer_capabilities.remove(&peer);
                    if let Some(service) = config.transfer.as_ref() {
                        service.set_peer_flow_support(peer, false);
                    }
                    if let Some(updates) = peer_connection_updates.get(&peer) {
                        updates.send_replace(None);
                    }
                    emit_peer_tunnels_starting(peer, &running_tunnels, &events).await;
                    peers.transition(peer, generation, PeerLifecycle::Disconnected);
                    emit_peer_state(&events, peer, generation, PeerLifecycle::Disconnected).await;
                }
                let canonical = canonical_candidates(&candidates);
                if let Some((generation, attempt)) = start_peer_attempt(
                    peer,
                    candidates,
                    token,
                    local_node,
                    identity.clone(),
                    &network,
                    &events,
                    &input_tx,
                    &semaphore,
                    &mut peers,
                    &mut pending_inbound,
                    &mut peer_tasks,
                    auth_context.clone(),
                    remote_verifier.clone(),
                    AttemptOptions {
                        capabilities: local_capabilities,
                    },
                    credentials.get(&peer).cloned().or_else(|| {
                        #[cfg(test)]
                        {
                            config.test_outgoing_password.clone()
                        }
                        #[cfg(not(test))]
                        {
                            None
                        }
                    }),
                )
                .await
                {
                    peer_attempts.insert(peer, (token, attempt));
                    peer_candidates.insert(peer, (generation, canonical));
                }
            }
            Wake::QueueChanged => {}
            Wake::Maintenance => {
                // Subscriptions are the actual active Agent-owned mappings, including
                // manually started rules. Stopped/deleted rules drop their receivers.
                let mut wanted = peer_connection_updates
                    .iter()
                    .filter(|(_, update)| update.receiver_count() > 0)
                    .map(|(peer, _)| *peer)
                    .collect::<HashSet<_>>();
                wanted.extend(requested_peers.iter().copied());
                for rule in tunnel_rules.values().filter(|r| r.enabled && r.auto_start) {
                    if let Ok(peer) = NodeId::from_hex(&rule.peer_node_id) {
                        wanted.insert(peer);
                    }
                }
                for peer in wanted {
                    if manually_disconnected.contains(&peer)
                        || resource_policy.borrow().for_peer(peer).disabled
                        || peers.state(peer).is_some_and(PeerLifecycle::is_active)
                        || (!credentials.contains_key(&peer)
                            && !trusted_devices.iter().any(|d| d.node_id == peer.to_hex())
                            && !credential_fallback)
                        || (signal.is_none()
                            && (!config.direct.enabled
                                || (direct_routes.get(&peer.to_hex()).is_none_or(Vec::is_empty)
                                    && nearby.addresses(peer).is_empty())))
                    {
                        continue;
                    }
                    if events.recovery.lock().unwrap().retry_due(peer) {
                        let _ = command_tx.try_send(SessionCommand::ConnectPeer(peer));
                    }
                }
                if let Some(service) = &config.transfer {
                    let (reconnect, jobs) = service.auto_due(
                        |peer, direction| {
                            let rights = authorizations.get(&peer).map(|(_, _, r)| *r.borrow());
                            connections
                                .get(&peer)
                                .is_some_and(|(_, c)| c.close_reason().is_none())
                                && peer_capabilities.get(&peer).is_some_and(|c| {
                                    c & crate::node::core::protocol::CAP_AUTO_RESUME != 0
                                })
                                && rights.is_some_and(|r| {
                                    r.outbound_authorized()
                                        && (direction
                                            == crate::node::core::task_model::TaskDirection::Send
                                            || r.inbound_authorized())
                                })
                        },
                        |peer| {
                            signal.is_none()
                                || peers.state(peer).is_some_and(PeerLifecycle::is_active)
                        },
                    );
                    for peer in reconnect {
                        let _ = command_tx.try_send(SessionCommand::ConnectPeer(peer));
                    }
                    for ticket in jobs {
                        let Some((_, connection)) = connections.get(&ticket.peer) else {
                            continue;
                        };
                        let live = authorizations[&ticket.peer].2.live();
                        let connection = connection.clone();
                        let service = service.clone();
                        peer_tasks.spawn(async move {
                            // Generation-owned rights abort the entire old continuation.
                            // A receive RPC also needs inbound rights for its new Offer.
                            if ticket.direction
                                == crate::node::core::task_model::TaskDirection::Receive
                            {
                                let incoming = live.clone();
                                let _ = live
                                    .guard(
                                        false,
                                        incoming
                                            .guard(true, service.auto_recover(&connection, ticket)),
                                    )
                                    .await;
                            } else {
                                let _ = live
                                    .guard(false, service.auto_recover(&connection, ticket))
                                    .await;
                            }
                        });
                    }
                }

                reconnect_probes.retain(|peer, watches| {
                    watches.retain(|watch| {
                        peers.is_current(*peer, watch.generation) && !watch.deadline.is_elapsed()
                    });
                    !watches.is_empty()
                });
                let expired = short_queries
                    .iter()
                    .filter(|(_, (_, started))| {
                        started.elapsed() >= crate::node::core::network_state::PEER_LOOKUP_TIMEOUT
                    })
                    .map(|(id, _)| *id)
                    .collect::<Vec<_>>();
                for short_id in expired {
                    short_queries.remove(&short_id);
                    let _ = events
                        .send(SessionEvent::ShortIdResolved {
                            short_id,
                            peer: None,
                        })
                        .await;
                }
                for (peer, generation) in peers.expired_lookups(time::Instant::now()) {
                    let state = PeerLifecycle::Failed("目标离线或候选等待超时，请重试连接".into());
                    if peers.transition(peer, generation, state.clone()) {
                        for (id, rule) in &waiting_tunnels {
                            if rule.peer_node_id == peer.to_hex() {
                                let _ = events
                                    .send(SessionEvent::TunnelState {
                                        rule_id: id.clone(),
                                        state: TunnelRuntimeState::Error(
                                            "目标离线或候选等待超时，请重试连接".into(),
                                        ),
                                    })
                                    .await;
                            }
                        }
                        fail_peer_tunnels(
                            peer,
                            "目标离线或候选等待超时，请重试连接",
                            &running_tunnels,
                            &mut forced_tunnel_errors,
                        );
                        queued_lookups.retain(|entry| *entry != (peer, generation));
                        emit_peer_state(&events, peer, generation, state).await;
                    }
                }
            }
            Wake::Input(None) => session_shutdown = true,
            Wake::Shutdown => session_shutdown = true,
            Wake::EventsClosed => session_shutdown = true,
            Wake::Retry => {
                reconnect_sleep = None;
                emit_lifecycle(
                    &events,
                    NetworkLifecycle::ReconnectingSignal {
                        attempt: reconnect_attempt,
                        delay: Duration::ZERO,
                    },
                )
                .await;
                start_signal_connect(
                    &identity,
                    (&signal_server, signal_tls.as_ref()),
                    &network.local_candidates,
                    signal_generation,
                    input_tx.clone(),
                    &mut connect_task,
                    &mut session_tasks,
                );
            }
            Wake::Task(Some(Err(error))) => {
                warn!(%error, "desktop session 子任务异常退出");
            }
            Wake::Task(Some(Ok(()))) | Wake::Task(None) => {}
            Wake::TunnelTask(Some(Ok((rule_id, result)))) => {
                running_tunnels.remove(&rule_id);
                if let Some(waiters) = tunnel_stop_waiters.remove(&rule_id) {
                    for done in waiters {
                        let _ = done.send(());
                    }
                }
                let result = forced_tunnel_errors.remove(&rule_id).map_or(result, Err);
                let state = match result {
                    Ok(()) if waiting_tunnels.contains_key(&rule_id) => {
                        TunnelRuntimeState::WaitingAuthorization
                    }
                    Ok(()) => TunnelRuntimeState::Stopped,
                    Err(detail) => TunnelRuntimeState::Error(detail),
                };
                let failed = matches!(state, TunnelRuntimeState::Error(_));
                let _ = events
                    .send(SessionEvent::TunnelState {
                        rule_id: rule_id.clone(),
                        state,
                    })
                    .await;
                let should_restart = restart_after_tunnel_stop.remove(&rule_id)
                    || waiting_tunnels
                        .get(&rule_id)
                        .and_then(|r| NodeId::from_hex(&r.peer_node_id).ok())
                        .is_some_and(|peer| {
                            peers
                                .state(peer)
                                .is_some_and(PeerLifecycle::outbound_authorized)
                        });
                if should_restart
                    && !failed
                    && let Some(rule) = tunnel_rules.get(&rule_id).cloned()
                {
                    start_tunnel_rule(
                        rule,
                        local_node,
                        &connections,
                        &peers,
                        &credentials,
                        credential_fallback,
                        &mut waiting_tunnels,
                        &peer_capabilities,
                        &mut peer_connection_updates,
                        &mut running_tunnels,
                        &mut tunnel_tasks,
                        &command_tx,
                        &events,
                        &tunnel_policy,
                        &forward_streams,
                    )
                    .await;
                }
            }
            Wake::TunnelTask(Some(Err(error))) => {
                warn!(%error, "本机转发 task 异常退出");
            }
            Wake::TunnelTask(None) => {}
        }
        if !session_shutdown && let Some(service) = config.transfer.as_ref() {
            let ready_connections = connections
                .iter()
                .filter(|(peer, (_, connection))| {
                    peers
                        .state(**peer)
                        .is_some_and(PeerLifecycle::outbound_authorized)
                        && connection.close_reason().is_none()
                        && (peer_capabilities.get(*peer).is_some_and(|caps| {
                            caps & crate::node::core::protocol::CAP_FILE_TRANSFER != 0
                        }))
                })
                .map(|(peer, (_, connection))| (*peer, connection.clone()))
                .collect();
            for scheduled in service.dispatch_ready(&ready_connections) {
                let live = authorizations[&scheduled.entry.peer].2.live();
                let connection = ready_connections[&scheduled.entry.peer].clone();
                let service = service.clone();
                let events = events.clone();
                // Same structured owner as receive/RPC workers: shutdown aborts
                // and drains every executor before persisted interruption recovery.
                peer_tasks.spawn(async move {
                    if let Err(error) = live
                        .guard(false, service.execute_queued(scheduled, connection))
                        .await
                    {
                        let _ = events
                            .send(SessionEvent::Diagnostic(error.to_string()))
                            .await;
                    }
                });
            }
        }
    }

    for (_, (_, _, rights)) in authorizations.drain() {
        rights.send_replace(RemoteAuthorization::default());
    }
    if let Some(task) = connect_task.take() {
        task.abort();
    }
    drop(signal);
    for running in running_tunnels.values() {
        running.shutdown.send_replace(true);
    }
    if time::timeout(Duration::from_secs(3), async {
        while tunnel_tasks.join_next().await.is_some() {}
    })
    .await
    .is_err()
    {
        tunnel_tasks.abort_all();
        while tunnel_tasks.join_next().await.is_some() {}
    }
    running_tunnels.clear();
    for (_, (_, connection)) in connections.drain() {
        connection.close(0u32.into(), b"desktop session shutdown");
    }
    network.close();
    forward_streams.shutdown().await;
    session_tasks.abort_all();
    peer_tasks.abort_all();
    while peer_tasks.join_next().await.is_some() {}
    while session_tasks.join_next().await.is_some() {}
    let _ = time::timeout(Duration::from_secs(1), network.wait_idle()).await;
}

fn retire_peer_listeners(
    peer: NodeId,
    running: &HashMap<String, RunningTunnel>,
    waiting: &mut HashMap<String, crate::node::core::config::TunnelRule>,
    restart: &mut HashSet<String>,
) {
    for (id, tunnel) in running {
        if tunnel.rule.peer_node_id == peer.to_hex() {
            waiting.insert(id.clone(), tunnel.rule.clone());
            restart.insert(id.clone());
            tunnel.shutdown.send_replace(true);
        }
    }
}

async fn emit_peer_tunnels_starting(
    peer: NodeId,
    running: &HashMap<String, RunningTunnel>,
    events: &SessionEvents,
) {
    for (rule_id, tunnel) in running {
        if NodeId::from_hex(&tunnel.rule.peer_node_id).ok() == Some(peer)
            && !*tunnel.shutdown.borrow()
        {
            let _ = events
                .send(SessionEvent::TunnelState {
                    rule_id: rule_id.clone(),
                    state: TunnelRuntimeState::Starting,
                })
                .await;
        }
    }
}

fn fail_peer_tunnels(
    peer: NodeId,
    detail: &str,
    running: &HashMap<String, RunningTunnel>,
    errors: &mut HashMap<String, String>,
) {
    for (id, tunnel) in running {
        if NodeId::from_hex(&tunnel.rule.peer_node_id).ok() == Some(peer) {
            errors.insert(id.clone(), format!("对端连接失败：{detail}"));
            tunnel.shutdown.send_replace(true);
        }
    }
}

struct TunnelServices {
    services: HashMap<String, String>,
}
#[allow(clippy::too_many_arguments)]
async fn start_tunnel_rule(
    rule: crate::node::core::config::TunnelRule,
    local_node: NodeId,
    connections: &HashMap<NodeId, (u64, quinn::Connection)>,
    peers: &PeerRegistry,
    _credentials: &HashMap<NodeId, SecretPassword>,
    _credential_fallback: bool,
    waiting_tunnels: &mut HashMap<String, crate::node::core::config::TunnelRule>,
    peer_capabilities: &HashMap<NodeId, u64>,
    peer_connection_updates: &mut HashMap<NodeId, watch::Sender<Option<quinn::Connection>>>,
    running_tunnels: &mut HashMap<String, RunningTunnel>,
    tunnel_tasks: &mut JoinSet<TunnelTaskResult>,
    command_tx: &mpsc::Sender<SessionCommand>,
    events: &SessionEvents,
    policy: &TunnelServices,
    stream_owner: &Arc<crate::tunnel::ForwardStreamOwner>,
) {
    if running_tunnels.contains_key(&rule.id) {
        return;
    }
    let rule_id = rule.id.clone();
    let _ = events
        .send(SessionEvent::TunnelState {
            rule_id: rule_id.clone(),
            state: TunnelRuntimeState::Starting,
        })
        .await;

    let peer = match NodeId::from_hex(&rule.peer_node_id) {
        Ok(peer) if peer != local_node => peer,
        Ok(_) => {
            let _ = events
                .send(SessionEvent::TunnelState {
                    rule_id,
                    state: TunnelRuntimeState::Error("转发规则不能绑定本机 Node ID".into()),
                })
                .await;
            return;
        }
        Err(error) => {
            let _ = events
                .send(SessionEvent::TunnelState {
                    rule_id,
                    state: TunnelRuntimeState::Error(format!("对端 Node ID 无效：{error}")),
                })
                .await;
            return;
        }
    };
    let service_id = {
        let Some(service) = policy.services.get(&rule_id) else {
            let _ = events
                .send(SessionEvent::TunnelState {
                    rule_id,
                    state: TunnelRuntimeState::Error(
                        "统一节点转发必须显式指定远端 service_id".into(),
                    ),
                })
                .await;
            return;
        };
        service.clone()
    };
    if !rule.enabled {
        let _ = events
            .send(SessionEvent::TunnelState {
                rule_id,
                state: TunnelRuntimeState::Error("本机转发规则已停用".into()),
            })
            .await;
        return;
    }
    if peer_capabilities
        .get(&peer)
        .is_some_and(|capabilities| capabilities & crate::node::core::protocol::CAP_TCP_TUNNEL == 0)
    {
        let _ = events
            .send(SessionEvent::TunnelState {
                rule_id,
                state: TunnelRuntimeState::Error(format!(
                    "对端设备 {} 不支持 TCP 隧道，请升级桌面端",
                    peer.short()
                )),
            })
            .await;
        return;
    }

    let outbound_authorized = peers
        .state(peer)
        .is_some_and(PeerLifecycle::outbound_authorized)
        && connections
            .get(&peer)
            .is_some_and(|(_, c)| c.close_reason().is_none());
    if !outbound_authorized {
        let _ = command_tx.try_send(SessionCommand::ConnectPeer(peer));
        waiting_tunnels.insert(rule_id.clone(), rule.clone());
        let _ = events
            .send(SessionEvent::TunnelState {
                rule_id,
                state: TunnelRuntimeState::WaitingAuthorization,
            })
            .await;
        return;
    }
    waiting_tunnels.remove(&rule_id);
    let listener = match tokio::net::TcpListener::bind(rule.listen).await {
        Ok(listener) => listener,
        Err(error) => {
            let _ = events
                .send(SessionEvent::TunnelState {
                    rule_id,
                    state: TunnelRuntimeState::Error(format!("监听 {} 失败：{error}", rule.listen)),
                })
                .await;
            return;
        }
    };
    let connection_updates = peer_connection_updates
        .entry(peer)
        .or_insert_with(|| watch::channel(None).0);
    if let Some((_, connection)) = connections.get(&peer).filter(|_| outbound_authorized) {
        connection_updates.send_replace(Some(connection.clone()));
    } else if let Err(error) = command_tx.try_send(SessionCommand::ConnectPeer(peer)) {
        let _ = events
            .send(SessionEvent::TunnelState {
                rule_id,
                state: TunnelRuntimeState::Error(format!("连接请求未排入队列：{error}")),
            })
            .await;
        return;
    }

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let connection_updates = connection_updates.subscribe();
    let mut state_updates = connection_updates.clone();
    let (errors_tx, mut errors_rx) = mpsc::channel(32);
    let task_events = events.clone();
    let task_rule_id = rule_id.clone();
    let stream_owner = stream_owner.clone();
    tunnel_tasks.spawn(async move {
        let forward = crate::tunnel::forward_with_owner(
            listener,
            rule.target,
            connection_updates,
            shutdown_rx,
            errors_tx,
            stream_owner,
            move |tcp, connection, _target| {
                let service_id = service_id.clone();
                async move {
                    let stream = crate::node::core::services::open_tcp(&connection, &service_id).await?;
                    crate::tunnel::bridge_tcp(tcp, stream.send, stream.recv).await
                }
            },
        );
        tokio::pin!(forward);
        let availability = |updates: &watch::Receiver<Option<quinn::Connection>>| {
            updates
                .borrow()
                .as_ref()
                .is_some_and(|connection| connection.close_reason().is_none())
        };
        let mut ready = availability(&state_updates);
        if ready {
            let _ = task_events
                .send(SessionEvent::TunnelState {
                    rule_id: task_rule_id.clone(),
                    state: TunnelRuntimeState::Running,
                })
                .await;
        }
        let deadline = time::sleep(TUNNEL_PEER_WAIT_TIMEOUT);
        tokio::pin!(deadline);
        let result = loop {
            tokio::select! {
                result = &mut forward => break result.map_err(|error| error.to_string()),
                _ = &mut deadline, if !ready => break Err("等待对端认证连接超时；本机监听已关闭，请重试启动".into()),
                changed = state_updates.changed() => {
                    if changed.is_err() { break Err("对端会话已结束".into()); }
                    ready = availability(&state_updates);
                    deadline.as_mut().reset(time::Instant::now() + TUNNEL_PEER_WAIT_TIMEOUT);
                    let state = if ready { TunnelRuntimeState::Running } else { TunnelRuntimeState::Starting };
                    let _ = task_events.send(SessionEvent::TunnelState { rule_id: task_rule_id.clone(), state }).await;
                },
                Some(detail) = errors_rx.recv() => {
                    let _ = task_events
                        .send(SessionEvent::TunnelError {
                            rule_id: task_rule_id.clone(),
                            detail,
                        })
                        .await;
                }
            }
        };
        (task_rule_id, result)
    });
    running_tunnels.insert(
        rule_id.clone(),
        RunningTunnel {
            rule,
            shutdown: shutdown_tx,
        },
    );
}

fn start_mapping_keepalives(
    network: &DesktopNetwork,
    tasks: &mut JoinSet<()>,
) -> Vec<tokio::task::AbortHandle> {
    network
        .paths
        .iter()
        .filter_map(|path| {
            // Keep the destination that produced our advertised IPv4 reflexive
            // address alive, even when no peers have a QUIC keepalive yet.
            let server = path.mapping_probe.as_ref()?.observations.first()?.server;
            let socket = path.punch_socket.clone();
            Some(tasks.spawn(async move {
                let mut interval = time::interval(Duration::from_secs(15));
                interval.set_missed_tick_behavior(time::MissedTickBehavior::Skip);
                loop {
                    interval.tick().await;
                    if let Err(error) = socket.keep_mapping_alive(server).await {
                        debug!(%error, "STUN mapping keepalive failed; retrying next interval");
                    }
                }
            }))
        })
        .collect()
}

fn start_signal_connect(
    identity: &Identity,
    target: (&str, Option<&crate::discovery::signal_tls::SignalTlsClient>),
    candidates: &[Candidate],
    generation: u64,
    inputs: mpsc::Sender<SessionInput>,
    task: &mut Option<tokio::task::AbortHandle>,
    tasks: &mut JoinSet<()>,
) {
    if target.0.trim().is_empty() {
        return;
    }
    let identity = identity.clone();
    let signal_server = target.0.to_owned();
    let signal_tls = target.1.cloned();
    let candidates = candidates.to_vec();
    *task = Some(tasks.spawn(async move {
        let result = SignalingClient::connect_desktop_with_tls(
            &signal_server,
            &identity,
            candidates,
            signal_tls.as_ref(),
        )
        .await;
        let _ = inputs
            .send(SessionInput::SignalConnected { generation, result })
            .await;
    }));
}

fn enqueue_pending_lookups(peers: &PeerRegistry, queued_lookups: &mut VecDeque<(NodeId, u64)>) {
    for pending in peers.pending_peers() {
        if !queued_lookups.contains(&pending) {
            queued_lookups.push_back(pending);
        }
    }
}

async fn schedule_reconnect(
    events: &SessionEvents,
    attempt: &mut u32,
    sleep: &mut Option<Pin<Box<Sleep>>>,
    detail: String,
) {
    let jitter = i32::from(rand::random::<u16>() % 401) - 200;
    let delay = reconnect_delay(*attempt, jitter);
    *attempt = attempt.saturating_add(1);
    *sleep = Some(Box::pin(time::sleep(delay)));
    let _ = events
        .send(SessionEvent::Diagnostic(format!(
            "信令已断开，将在 {:.1} 秒后重试：{detail}",
            delay.as_secs_f32()
        )))
        .await;
    emit_lifecycle(
        events,
        NetworkLifecycle::ReconnectingSignal {
            attempt: *attempt,
            delay,
        },
    )
    .await;
}

async fn emit_lifecycle(events: &SessionEvents, lifecycle: NetworkLifecycle) {
    let _ = events.send(SessionEvent::Lifecycle(lifecycle)).await;
}

async fn emit_peer_state(
    events: &SessionEvents,
    peer: NodeId,
    generation: u64,
    state: PeerLifecycle,
) {
    let _ = events
        .send(SessionEvent::PeerState {
            peer,
            generation,
            state,
        })
        .await;
}

/// A signed IPv6 candidate is only a routing hint for a bounded pending attempt,
/// never an identity or authorization. Full TLS-bound Ed25519 authentication of
/// the expected NodeId follows. IPv4 and unknown/ambiguous sources still need a probe.
fn pending_ipv6_route(
    remote: SocketAddr,
    local: NodeId,
    peers: &PeerRegistry,
    candidates: &HashMap<NodeId, (u64, Vec<Candidate>)>,
) -> Option<(NodeId, u64)> {
    if !remote.is_ipv6() || !crate::net::family::usable_address(remote) {
        return None;
    }
    let mut matches = candidates
        .iter()
        .filter_map(|(peer, (generation, addresses))| {
            (!should_initiate_quic(local, *peer)
                && peers.is_current(*peer, *generation)
                && matches!(
                    peers.state(*peer),
                    Some(PeerLifecycle::Punching | PeerLifecycle::Authenticating)
                )
                && addresses.iter().any(|candidate| candidate.addr == remote))
            .then_some((*peer, *generation))
        });
    let matched = matches.next()?;
    matches.next().is_none().then_some(matched)
}

fn recoverable_transport_close(reason: &quinn::ConnectionError) -> bool {
    matches!(
        reason,
        quinn::ConnectionError::TimedOut | quinn::ConnectionError::Reset
    ) || matches!(reason, quinn::ConnectionError::ApplicationClosed(close) if close.error_code == NETWORK_REFRESH_CLOSE_CODE.into())
}

async fn accept_incoming_loop(
    endpoint: quinn::Endpoint,
    family: AddressFamily,
    inputs: mpsc::Sender<SessionInput>,
) {
    while let Some(incoming) = endpoint.accept().await {
        if inputs
            .send(SessionInput::Incoming(family, Box::new(incoming)))
            .await
            .is_err()
        {
            return;
        }
    }
}

fn canonical_candidates(candidates: &[Candidate]) -> Vec<Candidate> {
    let mut result = candidates
        .iter()
        .copied()
        .filter(|c| c.kind != CandidateKind::Relay)
        .collect::<Vec<_>>();
    result.sort_by_key(|c| (c.kind, c.addr));
    result.dedup();
    result
}

#[derive(Clone, Copy)]
struct AttemptOptions {
    capabilities: u64,
}

#[allow(clippy::too_many_arguments)]
async fn start_peer_attempt(
    peer: NodeId,
    candidates: Vec<Candidate>,
    token: PunchToken,
    local_node: NodeId,
    identity: Identity,
    network: &DesktopNetwork,
    events: &SessionEvents,
    inputs: &mpsc::Sender<SessionInput>,
    semaphore: &Arc<Semaphore>,
    peers: &mut PeerRegistry,
    pending_inbound: &mut HashMap<(NodeId, u64), mpsc::Sender<quinn::Incoming>>,
    tasks: &mut JoinSet<()>,
    auth: AuthContext,
    verifier: Option<RemoteVerifier>,
    options: AttemptOptions,
    password: Option<SecretPassword>,
) -> Option<(u64, tokio::task::AbortHandle)> {
    if peer == local_node {
        return None;
    }
    let generation = match peers.state(peer) {
        Some(PeerLifecycle::PeerPending) => peers.generation(peer),
        Some(state) if state.is_active() => None,
        _ => match peers.begin_attempt(peer) {
            BeginPeerAttempt::Started(generation) => Some(generation),
            BeginPeerAttempt::AlreadyActive(_) => None,
            BeginPeerAttempt::AtCapacity => {
                let _ = events
                    .send(SessionEvent::Diagnostic("对端连接数已达资源上限".into()))
                    .await;
                return None;
            }
        },
    };
    let generation = generation?;

    let Ok(permit) = Arc::clone(semaphore).try_acquire_owned() else {
        let detail = "待认证连接已达资源上限".to_owned();
        peers.transition(peer, generation, PeerLifecycle::Failed(detail.clone()));
        emit_peer_state(
            events,
            peer,
            generation,
            PeerLifecycle::Failed(detail.clone()),
        )
        .await;
        let _ = events.send(SessionEvent::Diagnostic(detail)).await;
        return None;
    };

    let filtered: Vec<Candidate> = candidates
        .into_iter()
        .filter(|candidate| candidate.kind != CandidateKind::Relay)
        .collect::<Vec<_>>();
    let reachable = network.reachable_candidates(&filtered);
    let _ = events
        .send(SessionEvent::PeerCandidates {
            peer,
            generation,
            ipv4: filtered.iter().filter(|c| c.addr.is_ipv4()).count(),
            ipv6: filtered.iter().filter(|c| c.addr.is_ipv6()).count(),
        })
        .await;
    let _ = events
        .send(SessionEvent::PeerPath {
            peer,
            generation,
            detail: format!(
                "对端 {} 候选：IPv6 {} / IPv4 {}；本机可用路径 {}",
                peer.short(),
                filtered.iter().filter(|c| c.addr.is_ipv6()).count(),
                filtered.iter().filter(|c| c.addr.is_ipv4()).count(),
                network
                    .paths
                    .iter()
                    .map(|p| p.family.to_string())
                    .collect::<Vec<_>>()
                    .join(" / ")
            ),
        })
        .await;
    if reachable.is_empty() && network.relay.is_none() {
        let detail = "对端候选地址与本机 UDP 地址族不匹配".to_owned();
        peers.transition(peer, generation, PeerLifecycle::Failed(detail.clone()));
        emit_peer_state(
            events,
            peer,
            generation,
            PeerLifecycle::Failed(detail.clone()),
        )
        .await;
        return None;
    }

    if !peers.transition(peer, generation, PeerLifecycle::Punching) {
        return None;
    }
    emit_peer_state(events, peer, generation, PeerLifecycle::Punching).await;
    emit_lifecycle(events, NetworkLifecycle::Punching { peer }).await;
    let (inbound_tx, inbound_rx) = mpsc::channel(MAX_CANDIDATES);
    pending_inbound.insert((peer, generation), inbound_tx);
    let input_sender = inputs.clone();
    let relay = network.relay.clone().map(|mut relay| {
        let inputs = inputs.clone();
        relay.progress = Some(std::sync::Arc::new(move |detail| {
            let _ = inputs.try_send(SessionInput::PeerPath {
                peer,
                generation,
                detail: detail.to_owned(),
            });
        }));
        relay
    });
    let paths: Vec<_> = network
        .paths
        .iter()
        .filter_map(|path| {
            let candidates = path.reachable_candidates(&filtered);
            (!candidates.is_empty()).then(|| (path.clone(), candidates))
        })
        .collect();
    let attempt = tasks.spawn(async move {
        let result = run_peer_attempt(
            peer,
            generation,
            local_node,
            identity,
            paths,
            relay,
            token,
            inbound_rx,
            permit,
            input_sender.clone(),
            auth,
            verifier,
            options,
            password,
        )
        .await;
        if let Err(detail) = result {
            let _ = input_sender
                .send(SessionInput::PeerFailed {
                    peer,
                    generation,
                    detail,
                })
                .await;
        }
    });
    Some((generation, attempt))
}

#[allow(clippy::too_many_arguments)]
async fn run_peer_attempt(
    peer: NodeId,
    generation: u64,
    local_node: NodeId,
    identity: Identity,
    paths: Vec<(NetworkPath, Vec<SocketAddr>)>,
    relay: Option<crate::relay::client::Fallback>,
    token: PunchToken,
    inbound: mpsc::Receiver<quinn::Incoming>,
    _pending_permit: tokio::sync::OwnedSemaphorePermit,
    inputs: mpsc::Sender<SessionInput>,
    auth: AuthContext,
    verifier: Option<RemoteVerifier>,
    options: AttemptOptions,
    password: Option<SecretPassword>,
) -> std::result::Result<(), String> {
    let _ = inputs
        .send(SessionInput::PeerProgress {
            peer,
            generation,
            state: PeerLifecycle::Authenticating,
        })
        .await;
    let guard = race_peer_paths(
        paths,
        token,
        peer,
        generation,
        local_node,
        identity,
        inbound,
        relay.clone(),
    )
    .await
    .map_err(|error| error.to_string())?;
    finish_peer_transport(
        peer, generation, local_node, guard, relay, inputs, auth, verifier, options, password,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn finish_peer_transport(
    peer: NodeId,
    generation: u64,
    local_node: NodeId,
    mut guard: ConnectionGuard,
    relay: Option<crate::relay::client::Fallback>,
    inputs: mpsc::Sender<SessionInput>,
    auth: AuthContext,
    verifier: Option<RemoteVerifier>,
    options: AttemptOptions,
    password: Option<SecretPassword>,
) -> std::result::Result<(), String> {
    let connection = guard.connection();

    let _ = inputs
        .send(SessionInput::PeerProgress {
            peer,
            generation,
            state: PeerLifecycle::Negotiating,
        })
        .await;
    let capabilities = crate::node::core::wire::negotiate(
        connection,
        should_initiate_quic(local_node, peer),
        options.capabilities,
    )
    .await
    .map_err(|error| error.to_string())?;

    let _ = inputs
        .send(SessionInput::PeerProgress {
            peer,
            generation,
            state: PeerLifecycle::RemoteAuthPending,
        })
        .await;
    let authorization = match verifier {
        Some(verifier) => {
            auth.authorize_session(
                connection,
                should_initiate_quic(local_node, peer),
                local_node,
                peer,
                verifier,
                password,
                capabilities,
            )
            .await
        }
        None => Err(Error::Protocol("本机远程访问密码尚未初始化".into())),
    };
    let authorization = match authorization {
        Ok(authorization) => authorization,
        Err(error) => {
            connection.close(3u32.into(), b"remote authorization failed");
            return Err(error.to_string());
        }
    };
    // No business stream handler or listener availability is published before this point.
    if let Some(relay) = &relay {
        guard
            .retain_relay_endpoint(&relay.endpoints)
            .map_err(|e| e.to_string())?;
    }
    inputs
        .send(SessionInput::PeerConnected {
            peer,
            generation,
            capabilities,
            authorization: authorization.authorization,
            control: Box::new(authorization.control),
            connection: guard.release(),
        })
        .await
        .map_err(|_| "desktop session 已关闭".to_owned())
}

fn transport_can_publish(
    peers: &PeerRegistry,
    peer: NodeId,
    generation: u64,
    connection: &quinn::Connection,
) -> bool {
    if !peers.is_current(peer, generation)
        || peers.state(peer).is_some_and(PeerLifecycle::is_connected)
    {
        connection.close(0u32.into(), b"duplicate or stale peer generation");
        false
    } else {
        true
    }
}

#[allow(clippy::too_many_arguments)]
async fn race_peer_paths(
    paths: Vec<(NetworkPath, Vec<SocketAddr>)>,
    token: PunchToken,
    peer: NodeId,
    generation: u64,
    local: NodeId,
    identity: Identity,
    mut inbound: mpsc::Receiver<quinn::Incoming>,
    relay: Option<crate::relay::client::Fallback>,
) -> Result<ConnectionGuard> {
    let initiator = should_initiate_quic(local, peer);
    // Registration is synchronous and per path, before awaiting probes or any incoming connection.
    let mut routes = Vec::new();
    for (path, candidates) in paths {
        let receiver = path
            .punch_socket
            .register_peer_probe(&token, peer, generation)?;
        routes.push((path, candidates, receiver));
    }
    if initiator {
        let mut tasks = JoinSet::new();
        let has_v6 = routes
            .iter()
            .any(|(p, _, _)| p.family == AddressFamily::Ipv6);
        for (path, candidates, mut receiver) in routes {
            let identity = identity.clone();
            tasks.spawn(async move {
                if has_v6 && path.family == AddressFamily::Ipv4 {
                    time::sleep(crate::net::race::FAMILY_STAGGER).await;
                }
                let remote = if path.family == AddressFamily::Ipv6 {
                    match time::timeout(
                        crate::net::race::FAMILY_STAGGER,
                        punch_candidates(&path.punch_socket, &mut receiver, &candidates, &token),
                    )
                    .await
                    {
                        Ok(Ok(remote)) => remote,
                        _ => candidates[0], // IPv6 QUIC still verifies NodeId/TLS; a probe is optional.
                    }
                } else {
                    punch_candidates(&path.punch_socket, &mut receiver, &candidates, &token).await?
                };
                let mut ordered = vec![remote];
                for candidate in candidates {
                    if !ordered.contains(&candidate) {
                        ordered.push(candidate);
                    }
                }
                let mut dials = JoinSet::new();
                for (index, remote) in ordered.into_iter().enumerate() {
                    let identity = identity.clone();
                    let endpoint = path.endpoint.clone();
                    dials.spawn(async move {
                        time::sleep(crate::net::race::CANDIDATE_STAGGER * index as u32).await;
                        time::timeout(
                            crate::net::race::PATH_TIMEOUT,
                            PreparedTransport::dial(&endpoint, remote, &identity, peer),
                        )
                        .await
                        .map_err(|_| {
                            Error::Transport(format!("{remote} authenticated path timeout"))
                        })?
                    });
                }
                crate::net::race::select_prepared(dials).await
            });
        }
        if let Some(relay) = relay {
            tasks.spawn(async move { relay.prepare(identity, peer, token).await });
        }
        return crate::net::race::finish_prepared_guard(tasks).await;
    }
    let mut probes = JoinSet::new();
    for (path, candidates, mut receiver) in routes {
        probes.spawn(async move {
            let _ = punch_candidates(&path.punch_socket, &mut receiver, &candidates, &token).await;
            // Retain the token/source route even after a successful probe; Quinn's
            // incoming event is routed later by the generation-fenced actor.
            std::future::pending::<()>().await;
            drop(receiver);
        });
    }
    let mut authentication = JoinSet::new();
    let has_relay = relay.is_some();
    if let Some(relay) = relay {
        let relay_identity = identity.clone();
        authentication.spawn(async move { relay.wait(relay_identity, peer, token).await });
    }
    let deadline = time::sleep(if has_relay {
        crate::relay::RELAY_FALLBACK_DELAY
            + Duration::from_secs(3)
            + crate::relay::RELAY_ATTEMPT_TIMEOUT
            + APPLICATION_HANDSHAKE_TIMEOUT
    } else {
        QUIC_HANDSHAKE_TIMEOUT + APPLICATION_HANDSHAKE_TIMEOUT + Duration::from_secs(6)
    });
    tokio::pin!(deadline);
    let result = loop {
        tokio::select! {
            incoming = inbound.recv() => {
                let Some(incoming) = incoming else { break Err(Error::Transport("incoming generation retired".into())); };
                if authentication.len() >= MAX_CANDIDATES { incoming.refuse(); continue; }
                let identity = identity.clone();
                authentication.spawn(async move {
                    authenticate_incoming(incoming, &identity, peer).await.map(ConnectionGuard::new).map_err(Error::Transport)
                });
            }
            result = authentication.join_next(), if !authentication.is_empty() => {
                if let Some(Ok(Ok(winner))) = result { break Ok(winner); }
            }
            _ = &mut deadline => break Err(Error::Transport("all incoming paths timed out".into())),
        }
    };
    probes.abort_all();
    authentication.abort_all();
    while probes.join_next().await.is_some() {}
    while authentication.join_next().await.is_some() {}
    result
}

async fn punch_candidates(
    socket: &crate::transport::quic::PunchSocketHandle,
    events: &mut crate::transport::quic::PunchProbeReceiver,
    candidates: &[SocketAddr],
    token: &PunchToken,
) -> Result<SocketAddr> {
    if candidates.len() > MAX_CANDIDATES {
        return Err(Error::Transport("对端候选地址超过桌面连接上限".into()));
    }
    for attempt in 0..PEER_PUNCH_CONFIG.attempts {
        for candidate in candidates {
            if let Err(error) = socket.send_probe_to(token, *candidate).await {
                debug!(%candidate, %error, attempt, "发送桌面打洞探测包失败");
            }
        }
        match time::timeout(PEER_PUNCH_CONFIG.interval, events.recv()).await {
            Ok(Some(source)) => {
                // The first outbound probe may predate the remote token registration.
                // Reply to the validated actual source before stopping our probe loop.
                socket.send_probe_to(token, source).await?;
                return Ok(source);
            }
            Ok(None) => return Err(Error::Transport("打洞令牌接收器已关闭".into())),
            Err(_) => {}
        }
    }
    Err(Error::Transport(format!(
        "打洞超时：未收到带正确令牌的探测回应（{} 个候选）",
        candidates.len()
    )))
}

#[cfg(test)]
async fn authenticate_outgoing(
    endpoint: &quinn::Endpoint,
    remote: SocketAddr,
    identity: &Identity,
    expected_peer: NodeId,
) -> std::result::Result<quinn::Connection, String> {
    let connection = time::timeout(
        QUIC_HANDSHAKE_TIMEOUT,
        quic_connect(endpoint, remote, "p2pfile"),
    )
    .await
    .map_err(|_| "QUIC 握手超时".to_owned())?
    .map_err(|error| error.to_string())?;
    let binding =
        ChannelBinding::from_connection(&connection).map_err(|error| error.to_string())?;
    let (mut send, mut recv) = time::timeout(ACCEPT_FIRST_BI_STREAM_TIMEOUT, connection.open_bi())
        .await
        .map_err(|_| "打开 Ed25519 身份握手流超时".to_owned())?
        .map_err(|error| error.to_string())?;
    let outcome = time::timeout(
        APPLICATION_HANDSHAKE_TIMEOUT,
        handshake_initiator(&mut send, &mut recv, identity, &binding),
    )
    .await
    .map_err(|_| "Ed25519/channel-binding 发起方握手超时".to_owned())?
    .map_err(|error| error.to_string())?;
    let _ = send.finish();
    if outcome.peer_node_id != expected_peer {
        connection.close(2u32.into(), b"unexpected peer");
        return Err(format!(
            "对端身份不符：期待 {}，实际 {}",
            expected_peer.short(),
            outcome.peer_node_id.short()
        ));
    }
    Ok(connection)
}

async fn authenticate_incoming(
    incoming: quinn::Incoming,
    identity: &Identity,
    expected_peer: NodeId,
) -> std::result::Result<quinn::Connection, String> {
    let connection = time::timeout(QUIC_HANDSHAKE_TIMEOUT, incoming)
        .await
        .map_err(|_| "入站 QUIC 握手超时".to_owned())?
        .map_err(|error| error.to_string())?;
    let guard = ConnectionGuard::new(connection);
    let connection = guard.connection();
    let binding = ChannelBinding::from_connection(connection).map_err(|error| error.to_string())?;
    let (mut send, mut recv) =
        time::timeout(ACCEPT_FIRST_BI_STREAM_TIMEOUT, connection.accept_bi())
            .await
            .map_err(|_| "等待对端身份握手流超时".to_owned())?
            .map_err(|error| error.to_string())?;
    let outcome = time::timeout(
        APPLICATION_HANDSHAKE_TIMEOUT,
        handshake_responder(&mut send, &mut recv, identity, &binding),
    )
    .await
    .map_err(|_| "Ed25519/channel-binding 接收方握手超时".to_owned())?
    .map_err(|error| error.to_string())?;
    let _ = send.finish();
    if outcome.peer_node_id != expected_peer {
        connection.close(2u32.into(), b"unexpected peer");
        return Err(format!(
            "入站身份不符：期待 {}，实际 {}",
            expected_peer.short(),
            outcome.peer_node_id.short()
        ));
    }
    crate::transport::quic::await_identity_commit(&mut recv)
        .await
        .map_err(|e| e.to_string())?;
    Ok(guard.release())
}

#[cfg(test)]
fn test_password() -> SecretPassword {
    SecretPassword::new("Test9Pass".into()).unwrap()
}
#[cfg(test)]
fn test_verifier() -> RemoteVerifier {
    static VERIFIER: std::sync::OnceLock<RemoteVerifier> = std::sync::OnceLock::new();
    VERIFIER
        .get_or_init(|| RemoteVerifier::create(&test_password()).unwrap())
        .clone()
}

#[cfg(test)]
mod tests;
