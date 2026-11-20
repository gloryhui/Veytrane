//! Signaling server registry, admission, listeners, and connection handling.

use super::*;
use crate::discovery::short_id::{ALLOCATION_WINDOW_SECONDS, AllocationLimits, ShortIdStore};
use crate::discovery::signal_tls::SignalTlsServer;
use crate::identity::{public_key_from_bytes, signature_from_bytes, verify_signature};
use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, Semaphore, mpsc, watch};

const RECOVERY_CANDIDATE_TTL: Duration = Duration::from_secs(15);
const SHORT_LOOKUP_WINDOW: Duration = Duration::from_secs(10);
pub(super) const MAX_SHORT_LOOKUP_IPS: usize = 1024;
pub const NODE_LOOKUP_WINDOW: Duration = Duration::from_secs(10);
pub const DEFAULT_NODE_LOOKUPS_PER_CONNECTION: u32 = 64;
pub const DEFAULT_NODE_LOOKUPS_PER_IP: u32 = 256;
pub(super) const MAX_NODE_LOOKUP_IPS: usize = 1024;
pub const MAX_REGISTERED_PEERS: usize = 4096;
pub const MAX_WAITER_ENTRIES: usize = 16 * 1024;
pub const MAX_PENDING_LOOKUPS: usize = 64;
const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

/// 信令服务器资源与超时参数。
///
/// 默认值面向公网部署；测试用更小的值来快速验证边界行为。
#[derive(Clone, Debug)]
pub struct SignalServerConfig {
    pub tls: Option<SignalTlsServer>,
    pub tls_handshake_timeout: Duration,
    pub max_tls_handshakes: usize,
    /// None preserves the direct-only server startup and wire protocol.
    pub relay: Option<crate::relay::server::RelayServerConfig>,
    /// Persistent by default. None explicitly opts into an ephemeral embedded/test registry.
    pub short_id_database: Option<PathBuf>,
    /// New durable mappings only; reconnects do not consume these budgets.
    pub max_short_id_mappings: u32,
    pub short_id_allocations_per_minute: u32,
    pub short_id_allocations_per_ip_per_minute: u32,
    pub short_lookups_per_connection: u32,
    pub short_lookups_per_ip: u32,
    pub max_short_lookup_misses: u32,
    /// Ordinary NodeId requests, independently of offline waiter capacity.
    pub node_lookups_per_connection: u32,
    pub node_lookups_per_ip: u32,
    /// 单节点候选地址上限。
    pub max_candidates: usize,
    /// 在线节点总数上限。
    pub max_registered_peers: usize,
    /// waiter 条目总数上限。
    pub max_waiter_entries: usize,
    /// 单连接挂起查询上限。
    pub max_pending_lookups: usize,
    /// 每条连接待写队列容量。
    pub outbox_capacity: usize,
    /// 连接建立后，等第一帧的时间。
    pub first_frame_timeout: Duration,
    /// 发出 challenge 之后，等 `Register` 的时间。
    pub register_timeout: Duration,
    /// 登记成功后，两次收到帧之间的最大空闲时间。
    pub idle_timeout: Duration,
    /// 单帧写入超时。
    pub write_timeout: Duration,
    /// challenge 的有效期。
    pub challenge_ttl: Duration,
}

impl Default for SignalServerConfig {
    fn default() -> Self {
        Self {
            tls: None,
            tls_handshake_timeout: Duration::from_secs(10),
            max_tls_handshakes: 64,
            relay: None,
            short_id_database: Some(PathBuf::from("signal-device-ids.sqlite3")),
            max_short_id_mappings: 1_000_000,
            short_id_allocations_per_minute: 120,
            short_id_allocations_per_ip_per_minute: 10,
            short_lookups_per_connection: 5,
            short_lookups_per_ip: 20,
            max_short_lookup_misses: 3,
            node_lookups_per_connection: DEFAULT_NODE_LOOKUPS_PER_CONNECTION,
            node_lookups_per_ip: DEFAULT_NODE_LOOKUPS_PER_IP,
            max_candidates: MAX_CANDIDATES,
            max_registered_peers: MAX_REGISTERED_PEERS,
            max_waiter_entries: MAX_WAITER_ENTRIES,
            max_pending_lookups: MAX_PENDING_LOOKUPS,
            outbox_capacity: OUTBOX_CAPACITY,
            first_frame_timeout: Duration::from_secs(10),
            register_timeout: Duration::from_secs(10),
            idle_timeout: Duration::from_secs(180),
            write_timeout: Duration::from_secs(30),
            challenge_ttl: Duration::from_secs(10),
        }
    }
}
#[cfg(test)]
impl SignalServerConfig {
    pub(crate) fn for_tests() -> Self {
        Self {
            short_id_database: None,
            ..Self::default()
        }
    }
}

/// 一次成功登记的所有权凭证。
#[derive(Clone, Copy, Debug)]
struct ActiveRegistration {
    protocol_version: u32,
    node_id: NodeId,
    connection_id: u64,
    challenge: [u8; CHALLENGE_LEN],
    public_key: [u8; 32],
}

/// 正在等待 `Register` 的注册。
struct PendingRegistration {
    protocol_version: u32,
    node_id: NodeId,
    public_key: [u8; 32],
    challenge: [u8; CHALLENGE_LEN],
    issued_at: tokio::time::Instant,
}

/// 一个已登记的对端。
pub(super) struct PeerRecord {
    pub(super) connection_id: u64,
    pub(super) candidates: Vec<Candidate>,
    pub(super) outbox: mpsc::Sender<SignalMessage>,
    pub(super) close: watch::Sender<bool>,
}

/// 一个等待者：哪条**连接**在等（不只是哪个节点在等）。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(super) struct WaiterKey {
    pub(super) requester: NodeId,
    pub(super) connection_id: u64,
}

pub(super) struct RecoveryCandidates {
    pub(super) candidates: Vec<Candidate>,
    pub(super) expires_at: tokio::time::Instant,
}

/// 内存里的在线表。进程重启即清空——客户端会重新登记。
#[derive(Default)]
pub(super) struct Registry {
    pub(super) relay: Option<crate::relay::server::Admission>,
    pub(super) short_lookup_ips: HashMap<IpAddr, LookupWindow>,
    pub(super) node_lookup_ips: HashMap<IpAddr, LookupWindow>,
    pub(super) peers: HashMap<NodeId, PeerRecord>,
    pub(super) waiters: HashMap<NodeId, HashSet<WaiterKey>>,
    pub(super) pending_by_connection: HashMap<WaiterKey, HashSet<NodeId>>,
    pub(super) recovery: HashMap<WaiterKey, HashMap<NodeId, RecoveryCandidates>>,
}

#[derive(Debug)]
pub(super) struct LookupWindow {
    pub(super) started: tokio::time::Instant,
    pub(super) requests: u32,
    pub(super) misses: u32,
}

impl LookupWindow {
    pub(super) fn new() -> Self {
        Self {
            started: tokio::time::Instant::now(),
            requests: 0,
            misses: 0,
        }
    }
    fn expired(&self) -> bool {
        self.expired_after(SHORT_LOOKUP_WINDOW)
    }
    pub(super) fn expired_after(&self, window: Duration) -> bool {
        self.started.elapsed() >= window
    }
    fn take(&mut self, limit: u32, misses: u32) -> bool {
        self.take_in_window(limit, misses, SHORT_LOOKUP_WINDOW)
    }
    pub(super) fn take_in_window(&mut self, limit: u32, misses: u32, window: Duration) -> bool {
        if self.expired_after(window) {
            *self = Self::new();
        }
        if self.requests >= limit || self.misses >= misses {
            return false;
        }
        self.requests += 1;
        true
    }
}

#[derive(Clone)]
pub(super) struct ShortIdResources {
    store: Arc<ShortIdStore>,
    pub(super) pending: Arc<Semaphore>,
    pub(super) allocation_ips: Arc<std::sync::Mutex<HashMap<IpAddr, LookupWindow>>>,
    allocation_limits: AllocationLimits,
    per_ip_allocations: u32,
}

impl ShortIdResources {
    pub(super) fn new(config: &SignalServerConfig) -> Result<Self> {
        Ok(Self {
            store: Arc::new(ShortIdStore::open(config.short_id_database.as_deref())?),
            pending: Arc::new(Semaphore::new(config.max_pending_lookups)),
            allocation_ips: Arc::new(std::sync::Mutex::new(HashMap::new())),
            allocation_limits: AllocationLimits {
                max_mappings: config.max_short_id_mappings,
                per_window: config.short_id_allocations_per_minute,
            },
            per_ip_allocations: config.short_id_allocations_per_ip_per_minute,
        })
    }
    pub(super) async fn register(&self, node: NodeId, ip: IpAddr) -> Result<ShortId> {
        let permit = self
            .pending
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::Discovery("短 ID 数据库请求达到上限".into()))?;
        let store = self.store.clone();
        let ips = self.allocation_ips.clone();
        let limits = self.allocation_limits;
        let per_ip = self.per_ip_allocations;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            store.register_with_limits(node, limits, || {
                let window = Duration::from_secs(ALLOCATION_WINDOW_SECONDS as u64);
                let mut ips = ips
                    .lock()
                    .map_err(|_| Error::Discovery("发号预算不可用".into()))?;
                ips.retain(|_, bucket| !bucket.expired_after(window));
                if !ips.contains_key(&ip) && ips.len() >= MAX_SHORT_LOOKUP_IPS {
                    return Err(Error::Discovery("新设备分配暂时受限".into()));
                }
                if !ips
                    .entry(ip)
                    .or_insert_with(LookupWindow::new)
                    .take_in_window(per_ip, u32::MAX, window)
                {
                    return Err(Error::Discovery("新设备分配暂时受限".into()));
                }
                Ok(())
            })
        })
        .await
        .map_err(|_| Error::Discovery("短 ID 数据库任务失败".into()))?
    }
    pub(super) async fn lookup(&self, id: ShortId) -> Result<Option<NodeId>> {
        let permit = self
            .pending
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::Discovery(SHORT_LOOKUP_UNAVAILABLE.into()))?;
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            store.lookup(id)
        })
        .await
        .map_err(|_| Error::Discovery(SHORT_LOOKUP_UNAVAILABLE.into()))?
    }
}

impl Registry {
    fn store_recovery(
        &mut self,
        requester: WaiterKey,
        target: NodeId,
        candidates: Vec<Candidate>,
        limit: usize,
    ) -> bool {
        let now = tokio::time::Instant::now();
        let entries = self.recovery.entry(requester).or_default();
        entries.retain(|_, entry| entry.expires_at > now);
        if !entries.contains_key(&target) && entries.len() >= limit {
            if entries.is_empty() {
                self.recovery.remove(&requester);
            }
            return false;
        }
        entries.insert(
            target,
            RecoveryCandidates {
                candidates,
                expires_at: now + RECOVERY_CANDIDATE_TTL,
            },
        );
        true
    }

    fn candidates_for(&self, source: NodeId, target: NodeId, limit: usize) -> Vec<Candidate> {
        let Some(record) = self.peers.get(&source) else {
            return Vec::new();
        };
        let key = WaiterKey {
            requester: source,
            connection_id: record.connection_id,
        };
        let fresh = self
            .recovery
            .get(&key)
            .and_then(|entries| entries.get(&target));
        let Some(fresh) = fresh.filter(|entry| entry.expires_at > tokio::time::Instant::now())
        else {
            return record.candidates.clone();
        };
        let mut candidates = fresh.candidates.clone();
        candidates.extend_from_slice(&record.candidates);
        dedup_candidates(&mut candidates);
        candidates.truncate(limit.min(MAX_CANDIDATES));
        candidates
    }

    pub(super) fn take_node_lookup(&mut self, ip: IpAddr, config: &SignalServerConfig) -> bool {
        self.node_lookup_ips
            .retain(|_, bucket| !bucket.expired_after(NODE_LOOKUP_WINDOW));
        if !self.node_lookup_ips.contains_key(&ip)
            && self.node_lookup_ips.len() >= MAX_NODE_LOOKUP_IPS
        {
            return false;
        }
        self.node_lookup_ips
            .entry(ip)
            .or_insert_with(LookupWindow::new)
            .take_in_window(config.node_lookups_per_ip, u32::MAX, NODE_LOOKUP_WINDOW)
    }
    pub(super) fn take_short_lookup(&mut self, ip: IpAddr, config: &SignalServerConfig) -> bool {
        self.short_lookup_ips.retain(|_, window| !window.expired());
        if !self.short_lookup_ips.contains_key(&ip)
            && self.short_lookup_ips.len() >= MAX_SHORT_LOOKUP_IPS
        {
            return false;
        }
        self.short_lookup_ips
            .entry(ip)
            .or_insert_with(LookupWindow::new)
            .take(config.short_lookups_per_ip, u32::MAX)
    }
    pub(super) fn waiter_entries(&self) -> usize {
        self.waiters.values().map(HashSet::len).sum()
    }

    pub(super) fn pending_count(&self, key: WaiterKey) -> usize {
        self.pending_by_connection.get(&key).map_or(0, HashSet::len)
    }

    pub(super) fn add_waiter(&mut self, requester: WaiterKey, target: NodeId) -> bool {
        let inserted = self.waiters.entry(target).or_default().insert(requester);
        if inserted {
            self.pending_by_connection
                .entry(requester)
                .or_default()
                .insert(target);
        }
        inserted
    }

    pub(super) fn take_waiters_for(&mut self, target: NodeId) -> Vec<NodeId> {
        let Some(waiters) = self.waiters.remove(&target) else {
            return Vec::new();
        };

        let mut live = Vec::new();
        for key in waiters {
            if let Some(targets) = self.pending_by_connection.get_mut(&key) {
                targets.remove(&target);
                if targets.is_empty() {
                    self.pending_by_connection.remove(&key);
                }
            }
            let still_current = self
                .peers
                .get(&key.requester)
                .is_some_and(|record| record.connection_id == key.connection_id);
            if still_current && key.requester != target {
                live.push(key.requester);
            }
        }
        live
    }

    pub(super) fn try_pair(&self, a: NodeId, b: NodeId, max_candidates: usize) {
        let (Some(record_a), Some(record_b)) = (self.peers.get(&a), self.peers.get(&b)) else {
            return;
        };
        let candidates_a = self.candidates_for(a, b, max_candidates);
        let candidates_b = self.candidates_for(b, a, max_candidates);
        let outbox_a = record_a.outbox.clone();
        let outbox_b = record_b.outbox.clone();
        let close_a = record_a.close.clone();
        let close_b = record_b.close.clone();

        let permit_a = match outbox_a.try_reserve() {
            Ok(permit) => permit,
            Err(_) => {
                let _ = close_a.send(true);
                return;
            }
        };
        let permit_b = match outbox_b.try_reserve() {
            Ok(permit) => permit,
            Err(_) => {
                let _ = close_b.send(true);
                return;
            }
        };

        let token = PunchToken::random();
        if let Some(relay) = &self.relay
            && !relay.issue(token, a, b)
        {
            tracing::warn!("Relay ticket capacity unavailable; direct pairing still proceeds");
        }

        tracing::info!(
            a = %a.short(),
            b = %b.short(),
            a_candidates = candidates_a.len(),
            b_candidates = candidates_b.len(),
            token = %token,
            "牵线成功，双方开始打洞"
        );
        permit_a.send(SignalMessage::PeerCandidates {
            node_id: b,
            candidates: candidates_b,
            token,
        });
        permit_b.send(SignalMessage::PeerCandidates {
            node_id: a,
            candidates: candidates_a,
            token,
        });
    }

    pub(super) fn unregister(&mut self, node_id: NodeId, connection_id: u64) {
        let owned = self
            .peers
            .get(&node_id)
            .is_some_and(|record| record.connection_id == connection_id);
        if owned {
            self.peers.remove(&node_id);
            self.recovery.retain(|_, entries| {
                entries.remove(&node_id);
                !entries.is_empty()
            });
        }

        let key = WaiterKey {
            requester: node_id,
            connection_id,
        };
        self.recovery.remove(&key);
        if let Some(targets) = self.pending_by_connection.remove(&key) {
            for target in targets {
                if let Some(waiters) = self.waiters.get_mut(&target) {
                    waiters.remove(&key);
                }
            }
        }
        self.waiters.retain(|_, waiters| !waiters.is_empty());
    }
}

/// 跑信令服务器，永不返回（除非出错）。
pub async fn run_signal_server(listen: SocketAddr) -> Result<()> {
    let listener = TcpListener::bind(listen).await?;
    run_signal_server_on_with(
        listener,
        SignalServerConfig {
            short_id_database: Some(PathBuf::from("signal-device-ids.sqlite3")),
            ..SignalServerConfig::default()
        },
    )
    .await
}

/// 在**已经绑好**的监听器上跑信令服务器（默认配置）。
pub async fn run_signal_server_on(listener: TcpListener) -> Result<()> {
    run_signal_server_on_with(listener, SignalServerConfig::default()).await
}

/// 在已经绑好的监听器上跑信令服务器，使用给定配置。
pub async fn run_signal_server_on_with(
    listener: TcpListener,
    config: SignalServerConfig,
) -> Result<()> {
    run_signal_server_on_borrowed(&listener, config).await
}

// Borrowing also lets restart fixtures retain the same registered listener:
// duplicating and re-registering one underlying socket is not portable to IOCP.
pub(crate) async fn run_signal_server_on_borrowed(
    listener: &TcpListener,
    config: SignalServerConfig,
) -> Result<()> {
    let local = listener.local_addr()?;
    let tls = config
        .tls
        .as_ref()
        .map(SignalTlsServer::prepare)
        .transpose()?;
    if config.max_tls_handshakes == 0
        || config.max_tls_handshakes > 1024
        || config.tls_handshake_timeout.is_zero()
    {
        return Err(Error::Discovery("信令 TLS 握手资源配置无效".into()));
    }
    let handshakes = Arc::new(Semaphore::new(config.max_tls_handshakes));
    tracing::info!(tls = tls.is_some(), "信令连接模式（TLS 开启时禁止明文）");
    tracing::info!(%local, "信令服务器已启动，等待节点接入");

    let mut relay_tasks = tokio::task::JoinSet::new();
    let relay = if let Some(relay_config) = &config.relay {
        relay_config.validate()?;
        if relay_config.listen.is_empty() {
            return Err(Error::Discovery(
                "enabled Relay requires a UDP listener".into(),
            ));
        }
        let admission = crate::relay::server::Admission::new(relay_config.clone())?;
        let mut sockets = Vec::new();
        for address in &relay_config.listen {
            let socket = tokio::net::UdpSocket::from_std(crate::net::family::bind_udp(*address)?)?;
            sockets.push(socket);
        }
        relay_tasks.spawn(crate::relay::server::run_listeners(
            sockets,
            admission.clone(),
        ));
        Some(admission)
    } else {
        tracing::info!("Relay UDP disabled; signal exchanges candidates only");
        None
    };
    let registry = Arc::new(Mutex::new(Registry {
        relay,
        ..Registry::default()
    }));
    let short_ids = ShortIdResources::new(&config)?;
    let config = Arc::new(config);

    let mut next_connection_id: u64 = 1;
    let mut clients = tokio::task::JoinSet::new();

    loop {
        let accepted = tokio::select! {
            accepted = listener.accept() => accepted,
            _ = clients.join_next(), if !clients.is_empty() => continue,
            ended = relay_tasks.join_next(), if !relay_tasks.is_empty() => {
                return Err(Error::Discovery(format!("Relay UDP task stopped: {ended:?}")));
            },
        };
        let (stream, remote) = match accepted {
            Ok(pair) => pair,
            Err(err) => {
                tracing::warn!(error = %err, "接受连接失败，继续监听");
                continue;
            }
        };
        tracing::debug!(%remote, "信令连接建立");
        let permit = if tls.is_some() {
            match handshakes.clone().try_acquire_owned() {
                Ok(permit) => Some(permit),
                Err(_) => continue,
            }
        } else {
            None
        };

        let connection_id = next_connection_id;
        next_connection_id = next_connection_id.wrapping_add(1);
        if next_connection_id == 0 {
            next_connection_id = 1;
        }

        let registry = Arc::clone(&registry);
        let config = Arc::clone(&config);
        let short_ids = short_ids.clone();
        let tls = tls.clone();
        clients.spawn(async move {
            let result = async {
                if let Some(tls) = tls {
                    let _ = stream.set_nodelay(true);
                    let secured =
                        tokio::time::timeout(config.tls_handshake_timeout, tls.accept(stream))
                            .await
                            .map_err(|_| Error::Discovery("信令 TLS 握手超时".into()))?
                            .map_err(|_| {
                                Error::Discovery("信令 TLS 握手失败；该端口仅接受 TLS".into())
                            })?;
                    drop(permit);
                    handle_signal_io(
                        Box::new(secured),
                        remote.ip(),
                        registry,
                        connection_id,
                        config,
                        short_ids,
                    )
                    .await
                } else {
                    handle_signal_client(stream, registry, connection_id, config, short_ids).await
                }
            }
            .await;
            if let Err(err) = result {
                tracing::debug!(%remote, error = %err, "信令连接结束");
            }
        });
    }
}

struct AbortOnDrop(tokio::task::AbortHandle);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

fn reject(outbox: &mpsc::Sender<SignalMessage>, reason: impl Into<String>) {
    let _ = outbox.try_send(SignalMessage::Error {
        reason: reason.into(),
    });
}

fn send_or_close(outbox: &mpsc::Sender<SignalMessage>, message: SignalMessage) -> bool {
    outbox.try_send(message).is_ok()
}

pub(super) async fn handle_signal_client(
    stream: TcpStream,
    registry: Arc<Mutex<Registry>>,
    connection_id: u64,
    config: Arc<SignalServerConfig>,
    short_ids: ShortIdResources,
) -> Result<()> {
    let remote_ip = stream.peer_addr()?.ip();
    let _ = stream.set_nodelay(true);
    handle_signal_io(
        Box::new(stream),
        remote_ip,
        registry,
        connection_id,
        config,
        short_ids,
    )
    .await
}

async fn handle_signal_io(
    stream: SignalStream,
    remote_ip: IpAddr,
    registry: Arc<Mutex<Registry>>,
    connection_id: u64,
    config: Arc<SignalServerConfig>,
    short_ids: ShortIdResources,
) -> Result<()> {
    let mut short_window = LookupWindow::new();
    let mut node_window = LookupWindow::new();
    let (read_half, write_half) = tokio::io::split(stream);

    let (outbox, inbox) = mpsc::channel::<SignalMessage>(config.outbox_capacity);
    let (close_tx, mut close_rx) = watch::channel(false);
    let mut writer = tokio::spawn(writer_loop(write_half, inbox, Arc::clone(&config)));
    let _writer_guard = AbortOnDrop(writer.abort_handle());

    let mut reader = BufReader::new(read_half);
    let mut pending: Option<PendingRegistration> = None;
    let mut active: Option<ActiveRegistration> = None;
    let mut recovery_sequence = 0u64;

    let disconnect_reason: &str = loop {
        let read_timeout = if active.is_some() {
            config.idle_timeout
        } else if pending.is_some() {
            config.register_timeout
        } else {
            config.first_frame_timeout
        };

        let payload = tokio::select! {
            biased;
            _ = close_rx.changed() => break "服务器主动断开",
            result = tokio::time::timeout(
                read_timeout,
                read_raw_frame_limited(&mut reader, MAX_SIGNAL_FRAME_LEN),
            ) => {
                match result {
                    Err(_) => break "读取超时",
                    Ok(Ok(Some(payload))) => payload,
                    Ok(Ok(None)) => break "对端关闭连接",
                    Ok(Err(err)) => {
                        tracing::debug!(error = %err, "信令读取出错，按断开处理");
                        break "读取出错";
                    }
                }
            }
        };

        let message = match SignalMessage::decode(&payload) {
            Ok(message) => message,
            Err(err) => {
                tracing::warn!(error = %err, "信令消息解析失败");
                reject(&outbox, format!("消息解析失败: {err}"));
                break "消息解析失败";
            }
        };

        let (message, recovery_lookup) = match message {
            SignalMessage::RecoveryLookup {
                node_id,
                candidates,
                sequence,
                signature,
            } => (
                SignalMessage::Lookup { node_id },
                Some((candidates, sequence, signature)),
            ),
            other => (other, None),
        };

        match message {
            SignalMessage::RegisterHello {
                protocol_version,
                node_id,
                public_key,
            } => {
                if active.is_some() || pending.is_some() {
                    reject(&outbox, "本连接已经开始或完成注册，不能重复 RegisterHello");
                    break "重复 RegisterHello";
                }
                if ![
                    SIGNAL_PROTOCOL_VERSION,
                    SHORT_ID_PROTOCOL_VERSION,
                    RECOVERY_PROTOCOL_VERSION,
                ]
                .contains(&protocol_version)
                {
                    reject(
                        &outbox,
                        format!("信令协议版本不兼容：支持 2/3/4，对端 {protocol_version}"),
                    );
                    break "信令协议版本不兼容";
                }
                let key = match public_key_from_bytes(&public_key) {
                    Ok(key) => key,
                    Err(err) => {
                        reject(&outbox, format!("公钥非法: {err}"));
                        break "公钥非法";
                    }
                };
                if NodeId::from_public_key(&key) != node_id {
                    reject(&outbox, "节点 ID 与公钥不符");
                    break "节点 ID 与公钥不符";
                }

                let challenge: [u8; CHALLENGE_LEN] = rand::random();
                let issued_at = tokio::time::Instant::now();
                if !send_or_close(
                    &outbox,
                    SignalMessage::RegisterChallenge {
                        protocol_version,
                        challenge,
                    },
                ) {
                    break "待写队列已满";
                }
                pending = Some(PendingRegistration {
                    protocol_version,
                    node_id,
                    public_key,
                    challenge,
                    issued_at,
                });
            }

            SignalMessage::Register {
                node_id,
                public_key,
                candidates,
                signature,
            } => {
                let Some(pending_registration) = pending.take() else {
                    reject(&outbox, "没有先发 RegisterHello 就发 Register");
                    break "未按握手顺序注册";
                };
                if active.is_some() {
                    reject(&outbox, "本连接已经登记过，不能再次注册");
                    break "重复注册";
                }
                if pending_registration.node_id != node_id
                    || pending_registration.public_key != public_key
                {
                    reject(&outbox, "Register 与 RegisterHello 的身份不一致");
                    break "注册身份前后不一致";
                }
                if pending_registration.issued_at.elapsed() > config.challenge_ttl {
                    reject(&outbox, "challenge 已过期，请重新连接");
                    break "challenge 过期";
                }
                if candidates.len() > config.max_candidates {
                    reject(
                        &outbox,
                        format!(
                            "候选地址过多：{}，上限 {}",
                            candidates.len(),
                            config.max_candidates
                        ),
                    );
                    break "候选地址超限";
                }

                let key = match public_key_from_bytes(&public_key) {
                    Ok(key) => key,
                    Err(err) => {
                        reject(&outbox, format!("公钥非法: {err}"));
                        break "公钥非法";
                    }
                };
                if NodeId::from_public_key(&key) != node_id {
                    reject(&outbox, "节点 ID 与公钥不符");
                    break "节点 ID 与公钥不符";
                }
                let payload = match register_payload(
                    &pending_registration.challenge,
                    node_id,
                    &public_key,
                    &candidates,
                ) {
                    Ok(payload) => payload,
                    Err(err) => {
                        reject(&outbox, format!("无法构造注册载荷: {err}"));
                        break "注册载荷构造失败";
                    }
                };
                let signature = match signature_from_bytes(&signature) {
                    Ok(signature) => signature,
                    Err(err) => {
                        reject(&outbox, format!("签名非法: {err}"));
                        break "签名非法";
                    }
                };
                if let Err(err) = verify_signature(&key, &payload, &signature) {
                    reject(&outbox, format!("注册签名校验失败: {err}"));
                    break "注册签名校验失败";
                }

                {
                    let reg = registry.lock().await;
                    if !reg.peers.contains_key(&node_id)
                        && reg.peers.len() >= config.max_registered_peers
                    {
                        drop(reg);
                        reject(&outbox, "服务器在线表已满");
                        break "在线表已满";
                    }
                }
                let short_id = match short_ids.register(node_id, remote_ip).await {
                    Ok(id) => id,
                    Err(error) => {
                        reject(&outbox, error.to_string());
                        break "短 ID 持久化失败";
                    }
                };
                let (live_waiters, replaced_close) = {
                    let mut reg = registry.lock().await;
                    let is_new = !reg.peers.contains_key(&node_id);
                    if is_new && reg.peers.len() >= config.max_registered_peers {
                        drop(reg);
                        reject(&outbox, "服务器在线表已满");
                        break "在线表已满";
                    }
                    if !is_new {
                        reg.recovery.retain(|key, entries| {
                            entries.remove(&node_id);
                            key.requester != node_id && !entries.is_empty()
                        });
                    }
                    let replaced_close = reg
                        .peers
                        .insert(
                            node_id,
                            PeerRecord {
                                connection_id,
                                candidates: candidates.clone(),
                                outbox: outbox.clone(),
                                close: close_tx.clone(),
                            },
                        )
                        .map(|old| old.close);
                    let live = reg.take_waiters_for(node_id);
                    (live, replaced_close)
                };

                active = Some(ActiveRegistration {
                    protocol_version: pending_registration.protocol_version,
                    node_id,
                    connection_id,
                    challenge: pending_registration.challenge,
                    public_key,
                });

                tracing::info!(
                    node = %node_id.short(),
                    candidates = candidates.len(),
                    waiters = live_waiters.len(),
                    "节点登记（已通过私钥挑战认证）"
                );

                let registered =
                    if pending_registration.protocol_version >= SHORT_ID_PROTOCOL_VERSION {
                        SignalMessage::RegisteredShort { short_id }
                    } else {
                        SignalMessage::Registered
                    };
                if !send_or_close(&outbox, registered) {
                    break "待写队列已满";
                }

                if let Some(close) = replaced_close {
                    tracing::info!(node = %node_id.short(), "已被新连接取代，主动断开旧连接");
                    let _ = close.send(true);
                }

                for waiter in live_waiters {
                    registry
                        .lock()
                        .await
                        .try_pair(waiter, node_id, config.max_candidates);
                }
            }

            SignalMessage::LookupShort { short_id } => {
                let Some(registration) = active else {
                    reject(&outbox, SHORT_LOOKUP_UNAVAILABLE);
                    break "未注册短 ID 查询";
                };
                let owner = registry
                    .lock()
                    .await
                    .peers
                    .get(&registration.node_id)
                    .is_some_and(|record| record.connection_id == connection_id);
                if !owner || registration.protocol_version < SHORT_ID_PROTOCOL_VERSION {
                    reject(&outbox, SHORT_LOOKUP_UNAVAILABLE);
                    break "短 ID 查询身份/版本无效";
                }
                let allowed = short_window.take(
                    config.short_lookups_per_connection,
                    config.max_short_lookup_misses,
                ) && registry.lock().await.take_short_lookup(remote_ip, &config);
                let node_id = if allowed && ShortId::new(short_id.value()).is_ok() {
                    match short_ids.lookup(short_id).await {
                        Ok(node) => node,
                        Err(error) => {
                            tracing::warn!(%error, "短 ID 查询失败");
                            None
                        }
                    }
                } else {
                    None
                };
                if node_id.is_none() {
                    short_window.misses = short_window.misses.saturating_add(1);
                }
                if !send_or_close(&outbox, SignalMessage::ShortResolved { short_id, node_id }) {
                    break "待写队列已满";
                }
            }
            SignalMessage::Lookup { node_id: target } => {
                let Some(registration) = active.as_ref() else {
                    reject(&outbox, "还没登记就想查询");
                    break "未登记就查询";
                };
                let me = registration.node_id;
                let my_connection = registration.connection_id;

                let mut reg = registry.lock().await;
                let still_owner = reg
                    .peers
                    .get(&me)
                    .is_some_and(|record| record.connection_id == my_connection);
                if !still_owner {
                    drop(reg);
                    reject(&outbox, "本连接已被该节点的新连接取代");
                    break "连接所有权已被取代";
                }

                if !node_window.take_in_window(
                    config.node_lookups_per_connection,
                    u32::MAX,
                    NODE_LOOKUP_WINDOW,
                ) || !reg.take_node_lookup(remote_ip, &config)
                {
                    drop(reg);
                    if !send_or_close(
                        &outbox,
                        SignalMessage::Error {
                            reason: SHORT_LOOKUP_UNAVAILABLE.into(),
                        },
                    ) {
                        break "待写队列已满";
                    }
                    continue;
                }

                if me == target {
                    drop(reg);
                    if !send_or_close(
                        &outbox,
                        SignalMessage::Error {
                            reason: "别查自己".into(),
                        },
                    ) {
                        break "待写队列已满";
                    }
                    continue;
                }

                let recovery = if let Some((candidates, sequence, signature)) = recovery_lookup {
                    if registration.protocol_version != RECOVERY_PROTOCOL_VERSION
                        || candidates.len() > config.max_candidates.min(MAX_CANDIDATES)
                        || sequence <= recovery_sequence
                    {
                        drop(reg);
                        reject(&outbox, "恢复候选版本、数量或序号无效");
                        break "恢复候选无效";
                    }
                    let verified = public_key_from_bytes(&registration.public_key)
                        .and_then(|key| {
                            let signature = signature_from_bytes(&signature)?;
                            let payload = recovery_payload(
                                &registration.challenge,
                                target,
                                &candidates,
                                sequence,
                            )?;
                            verify_signature(&key, &payload, &signature)
                        })
                        .is_ok();
                    if !verified {
                        drop(reg);
                        reject(&outbox, "恢复候选签名无效");
                        break "恢复候选签名无效";
                    }
                    recovery_sequence = sequence;
                    Some(candidates)
                } else {
                    None
                };
                let key = WaiterKey {
                    requester: me,
                    connection_id: my_connection,
                };
                if reg.peers.contains_key(&target) {
                    if let Some(candidates) = recovery
                        && !reg.store_recovery(key, target, candidates, config.max_pending_lookups)
                    {
                        drop(reg);
                        reject(&outbox, "恢复候选数量已达上限");
                        continue;
                    }
                    reg.try_pair(me, target, config.max_candidates);
                    continue;
                }

                let already_waiting = reg
                    .waiters
                    .get(&target)
                    .is_some_and(|waiters| waiters.contains(&key));
                if !already_waiting {
                    if reg.pending_count(key) >= config.max_pending_lookups {
                        drop(reg);
                        if !send_or_close(
                            &outbox,
                            SignalMessage::Error {
                                reason: format!(
                                    "挂起的查询过多（上限 {}）",
                                    config.max_pending_lookups
                                ),
                            },
                        ) {
                            break "待写队列已满";
                        }
                        continue;
                    }
                    if reg.waiter_entries() >= config.max_waiter_entries {
                        drop(reg);
                        if !send_or_close(
                            &outbox,
                            SignalMessage::Error {
                                reason: "服务器等待队列已满".into(),
                            },
                        ) {
                            break "待写队列已满";
                        }
                        continue;
                    }
                }
                if let Some(candidates) = recovery
                    && !reg.store_recovery(key, target, candidates, config.max_pending_lookups)
                {
                    drop(reg);
                    reject(&outbox, "恢复候选数量已达上限");
                    continue;
                }
                if !already_waiting {
                    reg.add_waiter(key, target);
                }
                drop(reg);

                tracing::debug!(
                    node = %me.short(),
                    target = %target.short(),
                    "目标还没上线，挂起等待"
                );
                if !send_or_close(&outbox, SignalMessage::PeerPending { node_id: target }) {
                    break "待写队列已满";
                }
            }

            SignalMessage::Ping => {
                if active.is_none() {
                    reject(&outbox, "还没登记就想心跳");
                    break "未登记就心跳";
                }
                if !send_or_close(&outbox, SignalMessage::Pong) {
                    break "待写队列已满";
                }
            }

            other => {
                tracing::debug!(kind = other.kind(), "客户端发来了不该由它发送的消息");
                reject(&outbox, format!("服务器不接受 {} 消息", other.kind()));
                break "非法消息方向";
            }
        }
    };

    if let Some(registration) = active {
        registry
            .lock()
            .await
            .unregister(registration.node_id, registration.connection_id);
        tracing::info!(
            node = %registration.node_id.short(),
            reason = disconnect_reason,
            "节点下线"
        );
    }

    drop(outbox);
    if tokio::time::timeout(SHUTDOWN_GRACE, &mut writer)
        .await
        .is_err()
    {
        writer.abort();
    }
    Ok(())
}

async fn writer_loop(
    mut writer: SignalWrite,
    mut inbox: mpsc::Receiver<SignalMessage>,
    config: Arc<SignalServerConfig>,
) {
    while let Some(message) = inbox.recv().await {
        let payload = match message.encode() {
            Ok(payload) => payload,
            Err(err) => {
                tracing::warn!(error = %err, "信令消息编码失败");
                continue;
            }
        };
        match tokio::time::timeout(
            config.write_timeout,
            write_raw_frame_limited(&mut writer, &payload, MAX_SIGNAL_FRAME_LEN),
        )
        .await
        {
            Ok(Ok(())) => {}
            Ok(Err(err)) => {
                tracing::debug!(error = %err, "信令写失败，关闭连接");
                return;
            }
            Err(_) => {
                tracing::warn!("信令写超时，关闭异常连接");
                return;
            }
        }
    }
    let _ = writer.shutdown().await;
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::super::{
        CHALLENGE_LEN, Candidate, MAX_SIGNAL_FRAME_LEN, NodeId, PunchToken,
        SIGNAL_PROTOCOL_VERSION, SignalMessage, read_raw_frame_limited, register_payload,
        write_raw_frame_limited,
    };
    use super::{Registry, ShortIdResources, SignalServerConfig, handle_signal_client};
    use crate::identity::Identity;
    use std::net::SocketAddr;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::io::AsyncWriteExt;
    use tokio::net::{TcpListener, TcpStream};
    use tokio::sync::Mutex;

    /// 起一个真的信令服务器，返回它的地址。
    pub(crate) async fn spawn_signal_server() -> SocketAddr {
        spawn_signal_server_with(SignalServerConfig::for_tests())
            .await
            .0
    }

    /// 起一个带自定义配置的信令服务器，并把在线表暴露给测试。
    pub(crate) async fn spawn_signal_server_with(
        config: SignalServerConfig,
    ) -> (SocketAddr, Arc<Mutex<Registry>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let registry = Arc::new(Mutex::new(Registry::default()));
        let shared = Arc::clone(&registry);
        let short_ids = ShortIdResources::new(&config).unwrap();
        let config = Arc::new(config);
        let mut next_connection_id = 1u64;
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let registry = Arc::clone(&shared);
                let config = Arc::clone(&config);
                let short_ids = short_ids.clone();
                let connection_id = next_connection_id;
                next_connection_id = next_connection_id.wrapping_add(1);
                if next_connection_id == 0 {
                    next_connection_id = 1;
                }
                tokio::spawn(async move {
                    let _ =
                        handle_signal_client(stream, registry, connection_id, config, short_ids)
                            .await;
                });
            }
        });
        (addr, registry)
    }

    /// 轮询等待在线表满足条件（清理是连接 handler 异步做的）。
    pub(crate) async fn wait_for_registry<F>(registry: &Arc<Mutex<Registry>>, predicate: F)
    where
        F: Fn(&Registry) -> bool,
    {
        for _ in 0..200 {
            {
                let reg = registry.lock().await;
                if predicate(&reg) {
                    return;
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("等待在线表状态超时");
    }

    /// 裸信令连接：直接读写帧，用来构造畸形/攻击场景。
    pub(crate) struct Raw {
        pub(crate) stream: TcpStream,
    }

    impl Raw {
        pub(crate) async fn connect(addr: SocketAddr) -> Self {
            let stream = TcpStream::connect(addr).await.unwrap();
            stream.set_nodelay(true).unwrap();
            Self { stream }
        }

        pub(crate) async fn send(&mut self, message: &SignalMessage) {
            write_raw_frame_limited(
                &mut self.stream,
                &message.encode().unwrap(),
                MAX_SIGNAL_FRAME_LEN,
            )
            .await
            .unwrap();
        }

        /// 只写一个长度头，后面不跟载荷（用于验证长度上限）。
        pub(crate) async fn send_len_header(&mut self, len: u32) {
            self.stream.write_all(&len.to_le_bytes()).await.unwrap();
            self.stream.flush().await.unwrap();
        }

        pub(crate) async fn recv(&mut self) -> Option<SignalMessage> {
            let payload = read_raw_frame_limited(&mut self.stream, MAX_SIGNAL_FRAME_LEN)
                .await
                .unwrap()?;
            Some(SignalMessage::decode(&payload).unwrap())
        }

        /// 发 RegisterHello 并取回 challenge。
        pub(crate) async fn hello(&mut self, identity: &Identity) -> [u8; CHALLENGE_LEN] {
            self.send(&SignalMessage::RegisterHello {
                protocol_version: SIGNAL_PROTOCOL_VERSION,
                node_id: identity.node_id(),
                public_key: identity.public_key_bytes(),
            })
            .await;
            match self.recv().await {
                Some(SignalMessage::RegisterChallenge {
                    protocol_version,
                    challenge,
                }) => {
                    assert_eq!(protocol_version, SIGNAL_PROTOCOL_VERSION);
                    challenge
                }
                other => panic!("期待 RegisterChallenge，收到 {other:?}"),
            }
        }

        /// 用 `signer` 对 `identity` 的身份签名并发出 Register。
        pub(crate) async fn register_with(
            &mut self,
            identity: &Identity,
            signer: &Identity,
            challenge: &[u8; CHALLENGE_LEN],
            candidates: Vec<Candidate>,
        ) -> Option<SignalMessage> {
            let node_id = identity.node_id();
            let public_key = identity.public_key_bytes();
            let payload = register_payload(challenge, node_id, &public_key, &candidates).unwrap();
            self.send(&SignalMessage::Register {
                node_id,
                public_key,
                candidates,
                signature: signer.sign(&payload).to_bytes().to_vec(),
            })
            .await;
            self.recv().await
        }

        /// 走完一次合法注册。
        pub(crate) async fn full_register(
            &mut self,
            identity: &Identity,
            candidates: Vec<Candidate>,
        ) -> [u8; CHALLENGE_LEN] {
            let challenge = self.hello(identity).await;
            match self
                .register_with(identity, identity, &challenge, candidates)
                .await
            {
                Some(SignalMessage::Registered) => challenge,
                other => panic!("注册应当成功，实际 {other:?}"),
            }
        }
    }

    pub(crate) async fn node_lookup_reply(raw: &mut Raw, target: NodeId) -> SignalMessage {
        raw.send(&SignalMessage::Lookup { node_id: target }).await;
        tokio::time::timeout(Duration::from_secs(5), raw.recv())
            .await
            .unwrap()
            .unwrap()
    }

    pub(crate) async fn lookup_pair(
        a: &mut Raw,
        a_id: NodeId,
        b: &mut Raw,
        b_id: NodeId,
    ) -> PunchToken {
        let SignalMessage::PeerCandidates { node_id, token, .. } = node_lookup_reply(a, b_id).await
        else {
            panic!("lookup must pair")
        };
        assert_eq!(node_id, b_id);
        let other = tokio::time::timeout(Duration::from_secs(5), b.recv())
            .await
            .unwrap();
        assert!(
            matches!(other, Some(SignalMessage::PeerCandidates {node_id, token: other, ..}) if node_id == a_id && other == token)
        );
        token
    }

    pub(crate) async fn assert_target_has_no_candidates(raw: &mut Raw) {
        // A FIFO Ping barrier proves all earlier target pushes have been consumed,
        // without sleeping or assuming a quiet interval means no flood.
        raw.send(&SignalMessage::Ping).await;
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(5), raw.recv())
                .await
                .unwrap(),
            Some(SignalMessage::Pong)
        ));
    }
}
