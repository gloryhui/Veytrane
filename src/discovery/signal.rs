//! 公网信令：让两个素不相识的节点交换各自的候选地址。
//!
//! Direct 成功时服务器只负责信令，不承载业务数据。信令职责是：
//!
//! 1. 节点上线时登记自己的节点 ID 和候选地址；
//! 2. 一方查询另一方时，把双方候选互相推送；
//! 3. 之后双方优先自行直连；用户配置 Relay 且 Direct 未及时建立 authenticated
//!    transport 时，可选独立 Relay UDP task 延迟转发端到端加密的 QUIC datagram。
//!
//! Relay 看不到业务明文，QUIC / Ed25519 身份认证仍是设备间端到端。
//!
//! 信令走 TCP + 长度前缀 postcard；启用 Relay 还需放通配置的 UDP 端口。
//!
//! # 无论是否启用 TLS，登记都必须自证身份
//!
//! 信令默认明文 TCP，可选严格验证服务端的 TLS；登记仍使用签名证明客户端身份：
//!
//! ```text
//! 客户端                                  服务器
//!   RegisterHello { ver, id, pubkey }  ───►
//!                                       ◄─── RegisterChallenge { ver, nonce }
//!   Register { id, pubkey, cands, sig } ──►
//!                                       ◄─── Registered | Error
//! ```
//!
//! `sig` 覆盖 `domain + nonce + node_id + public_key + hash(candidates)`：
//!
//! - `node_id == hash(public_key)` 只证明「公钥自洽」，不证明持有私钥；
//!   签名才证明这一点。
//! - `nonce` 由服务器每条连接新生成、一次性使用，旧连接的注册消息原样重放会失败。
//! - 候选地址进签名：否则明文信令上的中间人可以篡改候选，把对端引到别处。
//!
//! 身份最终仍由 QUIC 之上的应用层握手确认；信令认证是为了让服务器自己的
//! 在线表不被随意污染（冒名、幽灵节点、内存耗尽）。

#[cfg(test)]
use std::collections::HashSet;
#[cfg(test)]
use std::net::IpAddr;
#[cfg(any(feature = "signaling-server", test))]
use std::net::SocketAddr;
use std::time::Duration;

use super::short_id::ShortId;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, ReadHalf, WriteHalf};
#[cfg(test)]
use tokio::net::TcpListener;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
#[cfg(test)]
use tokio::sync::watch;

use super::signal_tls::SignalTlsClient;
use crate::error::{Error, Result};
use crate::identity::{Identity, NodeId};
#[cfg(test)]
use crate::identity::{public_key_from_bytes, verify_signature};
use crate::nat::punch::PunchToken;
use crate::protocol::raw_frame::{read_raw_frame_limited, write_raw_frame_limited};
trait SignalIo: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> SignalIo for T {}
type SignalStream = Box<dyn SignalIo>;
type SignalRead = ReadHalf<SignalStream>;
type SignalWrite = WriteHalf<SignalStream>;

/// 信令服务器默认端口。
pub const DEFAULT_SIGNAL_PORT: u16 = 7000;

/// 信令单帧上限。
///
/// 信令全是小控制消息（候选地址最多 [`MAX_CANDIDATES`] 个），不该复用业务
/// 分片那 16 MiB 的通用上限，否则公网端口上一个长度头就能逼服务端提前分配大缓冲。
pub const MAX_SIGNAL_FRAME_LEN: u32 = 64 * 1024;

pub(crate) use veytrane_transport::signal_wire::recovery_payload;
pub use veytrane_transport::signal_wire::{
    CHALLENGE_LEN, Candidate, CandidateKind, MAX_CANDIDATES, RECOVERY_PROTOCOL_VERSION,
    REGISTER_DOMAIN, SHORT_ID_PROTOCOL_VERSION, SIGNAL_PROTOCOL_VERSION, SignalMessage,
    dedup_candidates, register_payload, sort_candidates,
};
const SHORT_LOOKUP_UNAVAILABLE: &str = "设备查询不可用或请求过多，请稍后重试";

/// 每条连接待写队列容量。
pub const OUTBOX_CAPACITY: usize = 64;

#[cfg(any(feature = "signaling-server", test))]
#[path = "../../apps/signaling/src/server.rs"]
mod server;

#[cfg(all(test, feature = "agent-base"))]
pub(crate) use server::run_signal_server_on_borrowed;
#[cfg(any(feature = "signaling-server", test))]
pub use server::{
    DEFAULT_NODE_LOOKUPS_PER_CONNECTION, DEFAULT_NODE_LOOKUPS_PER_IP, MAX_PENDING_LOOKUPS,
    MAX_REGISTERED_PEERS, MAX_WAITER_ENTRIES, NODE_LOOKUP_WINDOW, SignalServerConfig,
    run_signal_server, run_signal_server_on, run_signal_server_on_with,
};

// ---------------------------------------------------------------------------
// 信令客户端
// ---------------------------------------------------------------------------

/// 信令服务器给一对节点牵线后发下来的东西。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeerOffer {
    /// 对端公布的候选地址。
    pub candidates: Vec<Candidate>,
    /// 这一对节点的打洞令牌，双方拿到的必须一致。
    pub token: PunchToken,
}

/// `lookup` 的结果。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LookupOutcome {
    /// 对方已在线，直接拿到候选地址和打洞令牌。
    Ready(PeerOffer),
    /// 对方还没上线，等服务器推送。
    Pending,
}

/// 客户端自动心跳周期。
///
/// 服务器对已登记连接有 `idle_timeout`（默认 180s）。`SignalingClient` 会在这个
/// 周期内自动发 `Ping`，所以只要 `DirectLink` 还持有客户端，节点就一直在线——
/// 哪怕 QUIC/隧道长时间活跃、上层根本不去碰信令。
///
/// 必须明显小于服务器的 `idle_timeout`；服务器调小了这个值，就用
/// [`SignalingClient::connect_with_heartbeat`] 指定更短的周期。
pub const DEFAULT_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(60);

/// 客户端事件队列容量（reader 任务 → 前台 `lookup`/`wait_for_peer`）。
const CLIENT_EVENT_CAPACITY: usize = 256;
/// 客户端单帧写入超时，避免对端不读时 writer 任务无限卡住。
const CLIENT_WRITE_TIMEOUT: Duration = Duration::from_secs(30);

/// 信令客户端。
///
/// 登记完成后后台会跑两个任务：
///
/// - **writer**：所有出站消息走一条有界 channel，并**周期发心跳**；
/// - **reader**：持续读帧，`Pong` 直接丢掉（避免长时间不读把 TCP 缓冲撑满），
///   其余消息投给前台。
///
/// 这样上层在跑长时间隧道、完全不碰信令时，连接也不会因为空闲被服务器摘掉。
pub struct SignalingClient {
    node_id: NodeId,
    short_id: Option<ShortId>,
    server: String,
    /// 出站消息：前台方法 + 心跳任务都往这里投。
    outgoing: mpsc::Sender<SignalMessage>,
    /// reader 任务投递进来的服务器消息。
    events: mpsc::Receiver<SignalMessage>,
    recovery_signer: Option<RecoverySigner>,
    /// 只为了在 drop 时一起收掉后台任务（心跳随之停止）。
    _tasks: ClientTasks,
}

struct RecoverySigner {
    identity: Identity,
    challenge: [u8; CHALLENGE_LEN],
    sequence: std::sync::Mutex<u64>,
}

/// 后台任务句柄；`SignalingClient` drop 时一起收掉，心跳也随之停止。
struct ClientTasks {
    writer: tokio::task::JoinHandle<()>,
    reader: tokio::task::JoinHandle<()>,
}

impl Drop for ClientTasks {
    fn drop(&mut self) {
        self.writer.abort();
        self.reader.abort();
    }
}

impl SignalingClient {
    pub async fn connect_with_tls(
        server: &str,
        identity: &Identity,
        candidates: Vec<Candidate>,
        tls: Option<&SignalTlsClient>,
    ) -> Result<Self> {
        Self::connect_with_options(
            server,
            identity,
            candidates,
            Some(DEFAULT_HEARTBEAT_INTERVAL),
            false,
            SIGNAL_PROTOCOL_VERSION,
            tls,
        )
        .await
    }
    pub async fn connect_desktop_with_tls(
        server: &str,
        identity: &Identity,
        candidates: Vec<Candidate>,
        tls: Option<&SignalTlsClient>,
    ) -> Result<Self> {
        Self::connect_with_options(
            server,
            identity,
            candidates,
            Some(DEFAULT_HEARTBEAT_INTERVAL),
            true,
            RECOVERY_PROTOCOL_VERSION,
            tls,
        )
        .await
    }
    /// 连上信令服务器，通过 challenge-response 认证完成登记，并启动自动心跳。
    pub async fn connect(
        server: &str,
        identity: &Identity,
        candidates: Vec<Candidate>,
    ) -> Result<Self> {
        Self::connect_with_heartbeat(
            server,
            identity,
            candidates,
            Some(DEFAULT_HEARTBEAT_INTERVAL),
        )
        .await
    }

    /// 与 [`Self::connect`] 相同，但可以指定自动心跳周期（`None` 表示不心跳）。
    ///
    /// 测试用很短的周期验证「心跳让长连接跨过多个空闲周期仍在线」「关掉心跳后
    /// 才会被服务器清理」。
    pub async fn connect_with_heartbeat(
        server: &str,
        identity: &Identity,
        candidates: Vec<Candidate>,
        heartbeat_interval: Option<Duration>,
    ) -> Result<Self> {
        Self::connect_with_options(
            server,
            identity,
            candidates,
            heartbeat_interval,
            false,
            SIGNAL_PROTOCOL_VERSION,
            None,
        )
        .await
    }

    /// Connect with the normal registration/heartbeat behavior while exposing Pong
    /// events to the single event consumer. Desktop sessions should use this API and
    /// continuously drain [`Self::next_event`]; one-shot CLI users keep Pong filtering.
    pub async fn connect_with_events(
        server: &str,
        identity: &Identity,
        candidates: Vec<Candidate>,
    ) -> Result<Self> {
        Self::connect_with_options(
            server,
            identity,
            candidates,
            Some(DEFAULT_HEARTBEAT_INTERVAL),
            true,
            SIGNAL_PROTOCOL_VERSION,
            None,
        )
        .await
    }

    pub async fn connect_desktop_with_events(
        server: &str,
        identity: &Identity,
        candidates: Vec<Candidate>,
    ) -> Result<Self> {
        Self::connect_with_options(
            server,
            identity,
            candidates,
            Some(DEFAULT_HEARTBEAT_INTERVAL),
            true,
            RECOVERY_PROTOCOL_VERSION,
            None,
        )
        .await
    }

    async fn connect_with_options(
        server: &str,
        identity: &Identity,
        candidates: Vec<Candidate>,
        heartbeat_interval: Option<Duration>,
        forward_pongs: bool,
        protocol_version: u32,
        tls: Option<&SignalTlsClient>,
    ) -> Result<Self> {
        // 服务端也会拦，但客户端自己收一下，避免明知超限还去发一个大包。
        let mut candidates = candidates;
        if candidates.len() > MAX_CANDIDATES {
            tracing::warn!(
                count = candidates.len(),
                max = MAX_CANDIDATES,
                "候选地址过多，按上限截断后再登记"
            );
            candidates.truncate(MAX_CANDIDATES);
        }
        // 周期为 0 会 panic，按「不心跳」处理。
        let heartbeat_interval = heartbeat_interval.filter(|period| !period.is_zero());
        let tls = if let Some(options) = tls {
            let options = options.clone();
            let server = server.to_owned();
            Some(
                tokio::task::spawn_blocking(move || options.prepare(&server))
                    .await
                    .map_err(|_| Error::Discovery("信令 TLS 配置载入失败".into()))??,
            )
        } else {
            None
        };

        let stream = tokio::time::timeout(Duration::from_secs(10), TcpStream::connect(server))
            .await
            .map_err(|_| Error::Discovery(format!("连接信令服务器 {server} 超时")))?
            .map_err(|err| Error::Discovery(format!("连接信令服务器 {server} 失败: {err}")))?;
        let _ = stream.set_nodelay(true);

        let stream: SignalStream = if let Some((connector, name)) = tls {
            Box::new(
                tokio::time::timeout(Duration::from_secs(10), connector.connect(name, stream))
                    .await
                    .map_err(|_| Error::Discovery("信令 TLS 握手超时；未降级为明文".into()))?
                    .map_err(|error| {
                        Error::Discovery(format!(
                            "信令 TLS 握手或证书/主机名校验失败；未降级为明文：{error}"
                        ))
                    })?,
            )
        } else {
            Box::new(stream)
        };
        let (read_half, mut write_half) = tokio::io::split(stream);
        let mut reader = BufReader::new(read_half);
        let node_id = identity.node_id();

        // 整个注册交换（Hello → Challenge → Register → Registered）都要在
        // 超时内完成，避免服务器不回包时客户端无限等待。
        let (short_id, challenge) = tokio::time::timeout(
            Duration::from_secs(10),
            register_session(
                &mut reader,
                &mut write_half,
                identity,
                candidates,
                protocol_version,
            ),
        )
        .await
        .map_err(|_| Error::Discovery(format!("连接信令服务器 {server} 注册超时")))??;

        tracing::debug!(
            %server,
            node = %node_id.short(),
            heartbeat = ?heartbeat_interval,
            "已登记到信令服务器（已通过私钥挑战认证）"
        );

        let (outgoing, outbox) = mpsc::channel::<SignalMessage>(OUTBOX_CAPACITY);
        let (events_tx, events) = mpsc::channel::<SignalMessage>(CLIENT_EVENT_CAPACITY);

        let writer = tokio::spawn(client_writer_loop(write_half, outbox, heartbeat_interval));
        let reader_task = tokio::spawn(client_reader_loop(reader, events_tx, forward_pongs));

        Ok(Self {
            node_id,
            short_id,
            server: server.to_string(),
            outgoing,
            events,
            recovery_signer: (protocol_version == RECOVERY_PROTOCOL_VERSION).then(|| {
                RecoverySigner {
                    identity: identity.clone(),
                    challenge,
                    sequence: std::sync::Mutex::new(0),
                }
            }),
            _tasks: ClientTasks {
                writer,
                reader: reader_task,
            },
        })
    }

    pub fn short_id(&self) -> Option<ShortId> {
        self.short_id
    }

    pub async fn request_short_lookup(&self, short_id: ShortId) -> Result<()> {
        if self.short_id.is_none() {
            return Err(Error::Discovery("信令服务器/客户端不支持短设备 ID".into()));
        }
        self.outgoing
            .try_send(SignalMessage::LookupShort { short_id })
            .map_err(|_| Error::Discovery(SHORT_LOOKUP_UNAVAILABLE.into()))
    }

    pub fn node_id(&self) -> NodeId {
        self.node_id
    }

    pub fn server(&self) -> &str {
        &self.server
    }

    /// Enqueue an active peer lookup without consuming any response event.
    ///
    /// Long-lived desktop sessions use this together with [`Self::next_event`] so a
    /// single owner can route lookup responses, passive offers, heartbeat replies, and
    /// disconnects through one event loop. The legacy [`Self::lookup`] helper remains
    /// available to one-shot CLI callers.
    pub async fn request_lookup(&self, peer: NodeId) -> Result<()> {
        self.outgoing
            .try_send(SignalMessage::Lookup { node_id: peer })
            .map_err(|error| Error::Discovery(format!("信令查询队列不可用: {error}")))
    }

    pub(crate) async fn request_recovery_lookup(
        &self,
        peer: NodeId,
        candidates: &[Candidate],
    ) -> Result<()> {
        let signer = self
            .recovery_signer
            .as_ref()
            .ok_or_else(|| Error::Discovery("当前信令连接不支持恢复候选".into()))?;
        if candidates.len() > MAX_CANDIDATES {
            return Err(Error::Discovery("恢复候选地址过多".into()));
        }
        let mut sequence = signer
            .sequence
            .lock()
            .map_err(|_| Error::Discovery("恢复候选序号不可用".into()))?;
        let next = sequence
            .checked_add(1)
            .ok_or_else(|| Error::Discovery("恢复候选序号已耗尽".into()))?;
        let payload = recovery_payload(&signer.challenge, peer, candidates, next)?;
        let message = SignalMessage::RecoveryLookup {
            node_id: peer,
            candidates: candidates.to_vec(),
            sequence: next,
            signature: signer.identity.sign(&payload).to_bytes().to_vec(),
        };
        self.outgoing
            .try_send(message)
            .map_err(|error| Error::Discovery(format!("信令查询队列不可用: {error}")))?;
        *sequence = next;
        Ok(())
    }

    /// Receive the next server event from the sole signaling reader.
    ///
    /// Callers that use this API must route every returned event; selectively waiting
    /// for one peer and discarding other messages can lose passive offers.
    pub async fn next_event(&mut self) -> Result<SignalMessage> {
        self.events
            .recv()
            .await
            .ok_or_else(|| Error::Discovery("信令服务器断开了连接".into()))
    }

    /// 查询对端候选地址。
    pub async fn lookup(&mut self, peer: NodeId) -> Result<LookupOutcome> {
        self.send(SignalMessage::Lookup { node_id: peer }).await?;

        loop {
            let message = self
                .events
                .recv()
                .await
                .ok_or_else(|| Error::Discovery("等查询结果时信令服务器断开了连接".into()))?;

            match message {
                SignalMessage::PeerCandidates {
                    node_id,
                    candidates,
                    token,
                } if node_id == peer => {
                    return Ok(LookupOutcome::Ready(PeerOffer { candidates, token }));
                }
                SignalMessage::PeerPending { node_id } if node_id == peer => {
                    return Ok(LookupOutcome::Pending);
                }
                SignalMessage::Pong => continue,
                SignalMessage::Error { reason } => {
                    return Err(Error::Discovery(format!("查询失败: {reason}")));
                }
                other => {
                    tracing::debug!(kind = other.kind(), "查询期间忽略消息");
                }
            }
        }
    }

    /// 等服务器推送对端候选地址（配合 `lookup` 返回 `Pending` 使用）。
    /// 返回 `Ok(None)` 表示这段时间内对端没出现（超时），连接本身还是好的。
    pub async fn wait_for_peer(
        &mut self,
        peer: NodeId,
        timeout: Duration,
    ) -> Result<Option<PeerOffer>> {
        let deadline = tokio::time::Instant::now() + timeout;

        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                return Ok(None);
            }

            let message = match tokio::time::timeout(remaining, self.events.recv()).await {
                Err(_) => return Ok(None),
                Ok(None) => {
                    return Err(Error::Discovery("信令服务器断开了连接".into()));
                }
                Ok(Some(message)) => message,
            };

            match message {
                SignalMessage::PeerCandidates {
                    node_id,
                    candidates,
                    token,
                } if node_id == peer => {
                    return Ok(Some(PeerOffer { candidates, token }));
                }
                SignalMessage::Pong => continue,
                SignalMessage::Error { reason } => {
                    return Err(Error::Discovery(format!("信令错误: {reason}")));
                }
                other => {
                    tracing::debug!(kind = other.kind(), "等待期间忽略消息");
                }
            }
        }
    }

    /// 查询 + 必要时等待，一步到位。
    pub async fn resolve_peer(&mut self, peer: NodeId, timeout: Duration) -> Result<PeerOffer> {
        match self.lookup(peer).await? {
            LookupOutcome::Ready(offer) => Ok(offer),
            LookupOutcome::Pending => {
                tracing::info!(peer = %peer.short(), "对端还没上线，等待中");
                self.wait_for_peer(peer, timeout).await?.ok_or_else(|| {
                    Error::Discovery(format!(
                        "等对端 {} 上线超时（{} 秒）",
                        peer.short(),
                        timeout.as_secs()
                    ))
                })
            }
        }
    }

    /// 主动发个心跳，顺便确认出站方向还通（连接死了会返回错误）。
    ///
    /// 正常情况下不需要手动调用：后台已经在按
    /// [`DEFAULT_HEARTBEAT_INTERVAL`] 自动心跳。
    pub async fn ping(&mut self) -> Result<()> {
        self.send(SignalMessage::Ping).await
    }

    /// 往 writer 任务投递一条消息；writer 已经退出说明连接断了。
    async fn send(&self, message: SignalMessage) -> Result<()> {
        self.outgoing
            .send(message)
            .await
            .map_err(|_| Error::Discovery("信令连接已断开，无法发送".into()))
    }
}

/// 客户端 writer 任务：串行写出站消息，并周期发心跳。
async fn client_writer_loop(
    mut writer: SignalWrite,
    mut outgoing: mpsc::Receiver<SignalMessage>,
    heartbeat_interval: Option<Duration>,
) {
    let mut heartbeat = heartbeat_interval.map(|period| {
        let mut interval = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
        // 忙的时候不要补发一堆心跳，延迟到下一次即可。
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        interval
    });

    loop {
        let message = tokio::select! {
            maybe = outgoing.recv() => match maybe {
                Some(message) => message,
                // 所有发送端都没了（客户端被 drop）。
                None => break,
            },
            _ = async {
                match heartbeat.as_mut() {
                    Some(interval) => {
                        interval.tick().await;
                    }
                    None => std::future::pending::<()>().await,
                }
            } => SignalMessage::Ping,
        };

        let payload = match message.encode() {
            Ok(payload) => payload,
            Err(err) => {
                tracing::warn!(error = %err, "信令消息编码失败");
                continue;
            }
        };
        match tokio::time::timeout(
            CLIENT_WRITE_TIMEOUT,
            write_raw_frame_limited(&mut writer, &payload, MAX_SIGNAL_FRAME_LEN),
        )
        .await
        {
            Ok(Ok(())) => {}
            Ok(Err(err)) => {
                tracing::debug!(error = %err, "信令写失败，连接已结束");
                return;
            }
            Err(_) => {
                tracing::warn!("信令写超时，连接已结束");
                return;
            }
        }
    }

    let _ = writer.shutdown().await;
}

/// 客户端 reader 任务：持续读帧。
///
/// 关键在于**一直读**，避免心跳回包把 TCP 缓冲撑满。传统一次性调用方继续
/// 丢弃 Pong；长期桌面会话将 Pong 转交给唯一事件消费者。任务退出时 sender
/// 被 drop，前台等待者会看到 `None` 并报「连接已断开」。
async fn client_reader_loop(
    mut reader: BufReader<SignalRead>,
    events: mpsc::Sender<SignalMessage>,
    forward_pongs: bool,
) {
    loop {
        match read_raw_frame_limited(&mut reader, MAX_SIGNAL_FRAME_LEN).await {
            Ok(Some(payload)) => match SignalMessage::decode(&payload) {
                Ok(message) => {
                    if matches!(message, SignalMessage::Pong) && !forward_pongs {
                        continue;
                    }
                    // 队列满说明前台长期没消费；这里用 try_send 保证读取不被阻塞。
                    // 丢掉的推送最多是「别人查到了我们」，对端下次查询会重新牵线。
                    if events.try_send(message).is_err() {
                        if forward_pongs {
                            // Never silently lose a desktop offer. Surface disconnect;
                            // the session reconnects and retries pending discovery.
                            break;
                        }
                        tracing::debug!("客户端事件队列已满，丢弃一条服务器推送");
                    }
                }
                Err(err) => {
                    tracing::warn!(error = %err, "信令消息解析失败，连接已结束");
                    break;
                }
            },
            Ok(None) => break,
            Err(err) => {
                tracing::debug!(error = %err, "信令读取结束");
                break;
            }
        }
    }
}

/// 完成一次 challenge-response 注册（连接建立后的前两个来回）。
async fn register_session<R: AsyncRead + Unpin, W: AsyncWrite + Unpin>(
    reader: &mut BufReader<R>,
    writer: &mut W,
    identity: &Identity,
    candidates: Vec<Candidate>,
    protocol_version: u32,
) -> Result<(Option<ShortId>, [u8; CHALLENGE_LEN])> {
    let node_id = identity.node_id();
    let public_key = identity.public_key_bytes();

    let hello = SignalMessage::RegisterHello {
        protocol_version,
        node_id,
        public_key,
    };
    write_raw_frame_limited(writer, &hello.encode()?, MAX_SIGNAL_FRAME_LEN).await?;

    let reply = read_raw_frame_limited(reader, MAX_SIGNAL_FRAME_LEN)
        .await?
        .ok_or_else(|| Error::Discovery("信令服务器在 challenge 之前就关闭了连接".into()))?;
    let challenge = match SignalMessage::decode(&reply)? {
        SignalMessage::RegisterChallenge {
            protocol_version: remote_version,
            challenge,
        } => {
            if remote_version != protocol_version {
                return Err(Error::Discovery(format!(
                    "信令协议版本不兼容：本机 {protocol_version}，对端 {remote_version}"
                )));
            }
            challenge
        }
        SignalMessage::Error { reason } => {
            return Err(Error::Discovery(format!("登记被拒绝: {reason}")));
        }
        other => {
            return Err(Error::Discovery(format!(
                "期待 RegisterChallenge，收到 {}",
                other.kind()
            )));
        }
    };

    let payload = register_payload(&challenge, node_id, &public_key, &candidates)?;
    let register = SignalMessage::Register {
        node_id,
        public_key,
        candidates,
        signature: identity.sign(&payload).to_bytes().to_vec(),
    };
    write_raw_frame_limited(writer, &register.encode()?, MAX_SIGNAL_FRAME_LEN).await?;

    let reply = read_raw_frame_limited(reader, MAX_SIGNAL_FRAME_LEN)
        .await?
        .ok_or_else(|| Error::Discovery("信令服务器在确认前就关闭了连接".into()))?;
    match SignalMessage::decode(&reply)? {
        SignalMessage::Registered if protocol_version == SIGNAL_PROTOCOL_VERSION => {
            Ok((None, challenge))
        }
        SignalMessage::RegisteredShort { short_id }
            if protocol_version == SHORT_ID_PROTOCOL_VERSION
                || protocol_version == RECOVERY_PROTOCOL_VERSION =>
        {
            Ok((Some(ShortId::new(short_id.value())?), challenge))
        }
        SignalMessage::Error { reason } => Err(Error::Discovery(format!("登记被拒绝: {reason}"))),
        other => Err(Error::Discovery(format!(
            "登记后收到意外消息 {}",
            other.kind()
        ))),
    }
}

impl std::fmt::Debug for SignalingClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SignalingClient")
            .field("server", &self.server)
            .field("node_id", &self.node_id)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use server::{
        LookupWindow, MAX_NODE_LOOKUP_IPS, MAX_SHORT_LOOKUP_IPS, PeerRecord, Registry,
        ShortIdResources, WaiterKey,
        test_support::{
            Raw, assert_target_has_no_candidates, lookup_pair, node_lookup_reply,
            spawn_signal_server, spawn_signal_server_with, wait_for_registry,
        },
    };

    async fn short_reply(client: &mut SignalingClient) -> Option<NodeId> {
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if let SignalMessage::ShortResolved { node_id, .. } =
                    client.next_event().await.unwrap()
                {
                    return node_id;
                }
            }
        })
        .await
        .unwrap()
    }
    #[tokio::test]
    async fn short_ids_are_sequential_stable_and_query_resolves_real_identity() {
        let path =
            std::env::temp_dir().join(format!("p2p-signal-{:032x}.sqlite", rand::random::<u128>()));
        let config = SignalServerConfig {
            short_id_database: Some(path.clone()),
            ..SignalServerConfig::for_tests()
        };
        let (address, _) = spawn_signal_server_with(config.clone()).await;
        let a = Identity::generate();
        let b = Identity::generate();
        let mut first =
            SignalingClient::connect_desktop_with_events(&address.to_string(), &a, vec![])
                .await
                .unwrap();
        let second = SignalingClient::connect_desktop_with_events(&address.to_string(), &b, vec![])
            .await
            .unwrap();
        assert_eq!(first.short_id().unwrap().value(), 100_000_000);
        assert_eq!(second.short_id().unwrap().value(), 100_000_001);
        first
            .request_short_lookup(second.short_id().unwrap())
            .await
            .unwrap();
        assert_eq!(short_reply(&mut first).await, Some(b.node_id()));
        drop((first, second));
        // A new server owner with a fresh online registry uses the durable mapping.
        let (restarted, _) = spawn_signal_server_with(config).await;
        let again =
            SignalingClient::connect_desktop_with_events(&restarted.to_string(), &a, vec![])
                .await
                .unwrap();
        assert_eq!(again.short_id().unwrap().value(), 100_000_000);
        let renewed = SignalingClient::connect_desktop_with_events(
            &restarted.to_string(),
            &Identity::generate(),
            vec![],
        )
        .await
        .unwrap();
        assert_eq!(renewed.short_id().unwrap().value(), 100_000_002);
        let legacy =
            SignalingClient::connect(&restarted.to_string(), &Identity::generate(), vec![])
                .await
                .unwrap();
        assert!(legacy.short_id().is_none());
        assert!(
            legacy
                .request_short_lookup(ShortId::new(100_000_000).unwrap())
                .await
                .is_err()
        );
        drop((again, renewed, legacy));
        // Test helper owners retain open SQLite handles until runtime teardown (Windows).
        // File cleanup is deferred by using a temp directory outside the repository.
    }
    #[tokio::test]
    async fn short_lookup_connection_ip_unknown_scan_and_pending_budgets() {
        let config = SignalServerConfig {
            short_lookups_per_connection: 2,
            short_lookups_per_ip: 3,
            max_short_lookup_misses: 1,
            ..SignalServerConfig::for_tests()
        };
        let (address, _) = spawn_signal_server_with(config.clone()).await;
        let a = Identity::generate();
        let b = Identity::generate();
        let mut first =
            SignalingClient::connect_desktop_with_events(&address.to_string(), &a, vec![])
                .await
                .unwrap();
        let id = first.short_id().unwrap();
        first
            .request_short_lookup(ShortId::new(999_999_999).unwrap())
            .await
            .unwrap();
        assert_eq!(short_reply(&mut first).await, None);
        first.request_short_lookup(id).await.unwrap();
        assert_eq!(short_reply(&mut first).await, None);
        let mut second =
            SignalingClient::connect_desktop_with_events(&address.to_string(), &b, vec![])
                .await
                .unwrap();
        for _ in 0..2 {
            second.request_short_lookup(id).await.unwrap();
            assert_eq!(short_reply(&mut second).await, Some(a.node_id()));
        }
        second.request_short_lookup(id).await.unwrap();
        assert_eq!(short_reply(&mut second).await, None);
        let mut third = SignalingClient::connect_desktop_with_events(
            &address.to_string(),
            &Identity::generate(),
            vec![],
        )
        .await
        .unwrap();
        third.request_short_lookup(id).await.unwrap();
        assert_eq!(short_reply(&mut third).await, None);
        let resources = ShortIdResources::new(&SignalServerConfig {
            max_pending_lookups: 1,
            ..SignalServerConfig::for_tests()
        })
        .unwrap();
        let permit = resources.pending.clone().acquire_owned().await.unwrap();
        assert!(resources.lookup(id).await.is_err());
        drop(permit);
        assert!(resources.lookup(id).await.is_ok());
        let mut registry = Registry::default();
        for n in 0..MAX_SHORT_LOOKUP_IPS {
            let ip = IpAddr::V6(std::net::Ipv6Addr::from(n as u128));
            assert!(registry.take_short_lookup(ip, &config));
        }
        assert!(!registry.take_short_lookup("192.0.2.1".parse().unwrap(), &config));
    }
    #[tokio::test]
    async fn identity_churn_is_limited_after_disconnect_but_existing_identity_can_reconnect() {
        let (address, registry) = spawn_signal_server_with(SignalServerConfig {
            short_id_allocations_per_ip_per_minute: 2,
            ..SignalServerConfig::for_tests()
        })
        .await;
        let known = Identity::generate();
        for identity in [&known, &Identity::generate()] {
            let client = SignalingClient::connect_desktop_with_events(
                &address.to_string(),
                identity,
                vec![],
            )
            .await
            .unwrap();
            drop(client);
            wait_for_registry(&registry, |r| r.peers.is_empty()).await;
        }
        for _ in 0..8 {
            assert!(
                SignalingClient::connect_desktop_with_events(
                    &address.to_string(),
                    &Identity::generate(),
                    vec![]
                )
                .await
                .is_err()
            );
        }
        let reconnected =
            SignalingClient::connect_desktop_with_events(&address.to_string(), &known, vec![])
                .await
                .unwrap();
        assert_eq!(reconnected.short_id().unwrap().value(), 100_000_000);
    }

    #[tokio::test]
    async fn global_new_id_budget_covers_multiple_ips_and_source_table_stays_bounded() {
        let resources = ShortIdResources::new(&SignalServerConfig {
            short_id_allocations_per_minute: 2,
            short_id_allocations_per_ip_per_minute: 1,
            ..SignalServerConfig::for_tests()
        })
        .unwrap();
        let a = Identity::generate().node_id();
        let ip_a = "192.0.2.1".parse().unwrap();
        let first = resources.register(a, ip_a).await.unwrap();
        assert!(
            resources
                .register(Identity::generate().node_id(), ip_a)
                .await
                .is_err()
        );
        resources
            .register(Identity::generate().node_id(), "192.0.2.2".parse().unwrap())
            .await
            .unwrap();
        assert!(
            resources
                .register(Identity::generate().node_id(), "192.0.2.3".parse().unwrap())
                .await
                .is_err()
        );
        assert_eq!(
            resources
                .register(a, "192.0.2.3".parse().unwrap())
                .await
                .unwrap(),
            first
        );
        assert_eq!(resources.allocation_ips.lock().unwrap().len(), 2);
        let full = ShortIdResources::new(&SignalServerConfig {
            short_id_allocations_per_minute: u32::MAX,
            ..SignalServerConfig::for_tests()
        })
        .unwrap();
        let id = full.register(a, ip_a).await.unwrap();
        {
            let mut ips = full.allocation_ips.lock().unwrap();
            ips.clear();
            for n in 0..MAX_SHORT_LOOKUP_IPS {
                ips.insert(
                    IpAddr::V6(std::net::Ipv6Addr::from(n as u128)),
                    LookupWindow::new(),
                );
            }
        }
        assert!(
            full.register(Identity::generate().node_id(), "192.0.2.4".parse().unwrap())
                .await
                .is_err()
        );
        assert_eq!(
            full.allocation_ips.lock().unwrap().len(),
            MAX_SHORT_LOOKUP_IPS
        );
        // Existing mappings do not need a new source bucket even when its table is full.
        assert_eq!(
            full.register(a, "192.0.2.4".parse().unwrap())
                .await
                .unwrap(),
            id
        );
    }

    #[tokio::test]
    async fn full_online_registry_does_not_keep_consuming_durable_ids() {
        let (address, registry) = spawn_signal_server_with(SignalServerConfig {
            max_registered_peers: 1,
            ..SignalServerConfig::for_tests()
        })
        .await;
        let first = SignalingClient::connect_desktop_with_events(
            &address.to_string(),
            &Identity::generate(),
            vec![],
        )
        .await
        .unwrap();
        assert!(
            SignalingClient::connect_desktop_with_events(
                &address.to_string(),
                &Identity::generate(),
                vec![]
            )
            .await
            .is_err()
        );
        drop(first);
        wait_for_registry(&registry, |r| r.peers.is_empty()).await;
        let next = SignalingClient::connect_desktop_with_events(
            &address.to_string(),
            &Identity::generate(),
            vec![],
        )
        .await
        .unwrap();
        assert_eq!(next.short_id().unwrap().value(), 100_000_001);
    }
    #[tokio::test]
    async fn forged_registration_cannot_consume_a_short_id() {
        let (address, _) = spawn_signal_server_with(SignalServerConfig::for_tests()).await;
        let identity = Identity::generate();
        let attacker = Identity::generate();
        let mut raw = Raw::connect(address).await;
        let challenge = raw.hello(&identity).await;
        assert!(matches!(
            raw.register_with(&identity, &attacker, &challenge, vec![])
                .await,
            Some(SignalMessage::Error { .. })
        ));
        let valid =
            SignalingClient::connect_desktop_with_events(&address.to_string(), &identity, vec![])
                .await
                .unwrap();
        assert_eq!(valid.short_id().unwrap().value(), 100_000_000);
    }
    fn candidate(kind: CandidateKind, addr: &str) -> Candidate {
        Candidate::new(kind, addr.parse().unwrap())
    }

    #[tokio::test]
    async fn recovery_lookup_is_scoped_survives_reverse_lookup_and_supports_v3() {
        let (addr, _) = spawn_signal_server_with(SignalServerConfig::for_tests()).await;
        let alice = Identity::generate();
        let bob = Identity::generate();
        let carol = Identity::generate();
        let primary = candidate(CandidateKind::Host, "192.0.2.1:1000");
        let fresh = candidate(CandidateKind::Host, "192.0.2.1:2000");
        let mut a =
            SignalingClient::connect_desktop_with_events(&addr.to_string(), &alice, vec![primary])
                .await
                .unwrap();
        let mut b = SignalingClient::connect_with_options(
            &addr.to_string(),
            &bob,
            vec![],
            None,
            false,
            SHORT_ID_PROTOCOL_VERSION,
            None,
        )
        .await
        .unwrap();
        let mut c = SignalingClient::connect(&addr.to_string(), &carol, vec![])
            .await
            .unwrap();

        a.request_recovery_lookup(bob.node_id(), &[fresh])
            .await
            .unwrap();
        let a_offer = a.next_event().await.unwrap();
        let b_offer = b.next_event().await.unwrap();
        let (
            SignalMessage::PeerCandidates { token: a_token, .. },
            SignalMessage::PeerCandidates {
                candidates,
                token: b_token,
                ..
            },
        ) = (a_offer, b_offer)
        else {
            panic!("expected paired offers")
        };
        assert_eq!(a_token, b_token);
        assert_eq!(candidates, vec![fresh, primary]);

        b.request_lookup(alice.node_id()).await.unwrap();
        let b_offer = b.next_event().await.unwrap();
        let a_offer = a.next_event().await.unwrap();
        let (
            SignalMessage::PeerCandidates {
                candidates,
                token: b_token,
                ..
            },
            SignalMessage::PeerCandidates { token: a_token, .. },
        ) = (b_offer, a_offer)
        else {
            panic!("expected reverse paired offers")
        };
        assert_eq!(a_token, b_token);
        assert_eq!(candidates, vec![fresh, primary]);

        c.request_lookup(alice.node_id()).await.unwrap();
        let c_offer = c.next_event().await.unwrap();
        let a_offer = a.next_event().await.unwrap();
        let (
            SignalMessage::PeerCandidates {
                candidates,
                token: c_token,
                ..
            },
            SignalMessage::PeerCandidates { token: a_token, .. },
        ) = (c_offer, a_offer)
        else {
            panic!("expected third-party offers")
        };
        assert_eq!(a_token, c_token);
        assert_eq!(candidates, vec![primary]);
    }

    #[tokio::test]
    async fn recovery_lookup_offline_waiter_respects_expiry_and_owner() {
        let (addr, registry) = spawn_signal_server_with(SignalServerConfig::for_tests()).await;
        let alice = Identity::generate();
        let bob = Identity::generate();
        let primary = candidate(CandidateKind::Host, "192.0.2.1:1000");
        let fresh = candidate(CandidateKind::Host, "192.0.2.1:2000");
        let mut a =
            SignalingClient::connect_desktop_with_events(&addr.to_string(), &alice, vec![primary])
                .await
                .unwrap();
        a.request_recovery_lookup(bob.node_id(), &[fresh])
            .await
            .unwrap();
        assert!(matches!(
            a.next_event().await.unwrap(),
            SignalMessage::PeerPending { .. }
        ));
        let mut b = SignalingClient::connect(&addr.to_string(), &bob, vec![])
            .await
            .unwrap();
        let a_offer = a.next_event().await.unwrap();
        let b_offer = b.next_event().await.unwrap();
        let (
            SignalMessage::PeerCandidates { token: a_token, .. },
            SignalMessage::PeerCandidates {
                candidates,
                token: b_token,
                ..
            },
        ) = (a_offer, b_offer)
        else {
            panic!("expected offline waiter offers")
        };
        assert_eq!(a_token, b_token);
        assert_eq!(candidates, vec![fresh, primary]);

        {
            let mut reg = registry.lock().await;
            for entries in reg.recovery.values_mut() {
                for entry in entries.values_mut() {
                    entry.expires_at = tokio::time::Instant::now();
                }
            }
        }
        b.request_lookup(alice.node_id()).await.unwrap();
        assert!(matches!(b.next_event().await.unwrap(),
            SignalMessage::PeerCandidates { candidates, .. } if candidates == vec![primary]));
        let _ = a.next_event().await.unwrap();

        a.request_recovery_lookup(bob.node_id(), &[fresh])
            .await
            .unwrap();
        let _ = a.next_event().await.unwrap();
        let _ = b.next_event().await.unwrap();
        let replacement =
            SignalingClient::connect_desktop_with_events(&addr.to_string(), &alice, vec![primary])
                .await
                .unwrap();
        b.request_lookup(alice.node_id()).await.unwrap();
        assert!(matches!(b.next_event().await.unwrap(),
            SignalMessage::PeerCandidates { candidates, .. } if candidates == vec![primary]));
        drop(replacement);
    }

    #[tokio::test]
    async fn recovery_lookup_rejects_replay_and_tampered_candidates() {
        for tamper in [false, true] {
            let (addr, _) = spawn_signal_server_with(SignalServerConfig::for_tests()).await;
            let alice = Identity::generate();
            let target = Identity::generate().node_id();
            let fresh = candidate(CandidateKind::Host, "192.0.2.1:2000");
            let mut raw = Raw::connect(addr).await;
            raw.send(&SignalMessage::RegisterHello {
                protocol_version: RECOVERY_PROTOCOL_VERSION,
                node_id: alice.node_id(),
                public_key: alice.public_key_bytes(),
            })
            .await;
            let Some(SignalMessage::RegisterChallenge { challenge, .. }) = raw.recv().await else {
                panic!("expected challenge")
            };
            assert!(matches!(
                raw.register_with(&alice, &alice, &challenge, vec![]).await,
                Some(SignalMessage::RegisteredShort { .. })
            ));
            let signature = alice
                .sign(&recovery_payload(&challenge, target, &[fresh], 1).unwrap())
                .to_bytes()
                .to_vec();
            let message = SignalMessage::RecoveryLookup {
                node_id: target,
                candidates: if tamper { vec![] } else { vec![fresh] },
                sequence: 1,
                signature,
            };
            raw.send(&message).await;
            if !tamper {
                assert!(matches!(
                    raw.recv().await,
                    Some(SignalMessage::PeerPending { .. })
                ));
                raw.send(&message).await;
            }
            assert!(matches!(
                raw.recv().await,
                Some(SignalMessage::Error { .. })
            ));
        }
    }

    #[tokio::test]
    async fn recovery_lookup_obeys_pending_and_rate_limits() {
        for rate_limited in [false, true] {
            let config = SignalServerConfig {
                max_pending_lookups: 1,
                node_lookups_per_connection: if rate_limited { 1 } else { 64 },
                ..SignalServerConfig::for_tests()
            };
            let (addr, registry) = spawn_signal_server_with(config).await;
            let alice = Identity::generate();
            let first = Identity::generate().node_id();
            let second = Identity::generate().node_id();
            let fresh = candidate(CandidateKind::Host, "192.0.2.1:2000");
            let mut a =
                SignalingClient::connect_desktop_with_events(&addr.to_string(), &alice, vec![])
                    .await
                    .unwrap();
            a.request_recovery_lookup(first, &[fresh]).await.unwrap();
            assert!(matches!(
                a.next_event().await.unwrap(),
                SignalMessage::PeerPending { .. }
            ));
            a.request_recovery_lookup(second, &[fresh]).await.unwrap();
            assert!(matches!(
                a.next_event().await.unwrap(),
                SignalMessage::Error { .. }
            ));
            let reg = registry.lock().await;
            let records = reg.recovery.values().next().unwrap();
            assert!(records.contains_key(&first));
            assert!(!records.contains_key(&second));
        }
    }

    #[tokio::test]
    async fn recovery_lookup_prioritizes_fresh_candidates_within_limit() {
        let config = SignalServerConfig {
            max_candidates: 2,
            ..SignalServerConfig::for_tests()
        };
        let (addr, _) = spawn_signal_server_with(config).await;
        let alice = Identity::generate();
        let bob = Identity::generate();
        let primary = candidate(CandidateKind::Host, "192.0.2.1:1000");
        let fresh = candidate(CandidateKind::Host, "192.0.2.1:2000");
        let a = SignalingClient::connect_desktop_with_events(
            &addr.to_string(),
            &alice,
            vec![primary, fresh],
        )
        .await
        .unwrap();
        let mut b = SignalingClient::connect(&addr.to_string(), &bob, vec![])
            .await
            .unwrap();
        a.request_recovery_lookup(bob.node_id(), &[fresh])
            .await
            .unwrap();
        assert!(matches!(
            b.next_event().await.unwrap(),
            SignalMessage::PeerCandidates { candidates, .. } if candidates == vec![fresh, primary]
        ));
    }

    #[tokio::test]
    async fn node_lookup_connection_limit_stops_pairing_tickets_and_target_flood_and_recovers() {
        for enable_relay in [false, true] {
            let (addr, registry) = spawn_signal_server_with(SignalServerConfig {
                node_lookups_per_connection: 3,
                node_lookups_per_ip: 20,
                ..SignalServerConfig::for_tests()
            })
            .await;
            let admission = crate::relay::server::Admission::new(Default::default()).unwrap();
            if enable_relay {
                registry.lock().await.relay = Some(admission.clone());
            }
            let a = Identity::generate();
            let b = Identity::generate();
            let mut ra = Raw::connect(addr).await;
            let mut rb = Raw::connect(addr).await;
            ra.full_register(&a, vec![]).await;
            rb.full_register(&b, vec![]).await;
            let mut tokens = HashSet::new();
            for _ in 0..3 {
                let token = lookup_pair(&mut ra, a.node_id(), &mut rb, b.node_id()).await;
                assert!(tokens.insert(*token.as_bytes()));
            }
            let ticket_count = if enable_relay { 3 } else { 0 };
            assert_eq!(admission.ticket_count(), ticket_count);
            for target in [b.node_id(), Identity::generate().node_id(), a.node_id()]
                .into_iter()
                .cycle()
                .take(12)
            {
                assert!(
                    matches!(node_lookup_reply(&mut ra, target).await, SignalMessage::Error {reason} if reason == SHORT_LOOKUP_UNAVAILABLE)
                );
            }
            assert_target_has_no_candidates(&mut rb).await;
            assert_eq!(admission.ticket_count(), ticket_count);
            assert_eq!(registry.lock().await.waiter_entries(), 0);
            // Both connection and IP windows expire; an actual Lookup pairs again.
            tokio::time::pause();
            tokio::time::advance(NODE_LOOKUP_WINDOW).await;
            tokio::time::resume();
            let token = lookup_pair(&mut ra, a.node_id(), &mut rb, b.node_id()).await;
            assert!(tokens.insert(*token.as_bytes()));
            assert_eq!(admission.ticket_count(), if enable_relay { 4 } else { 0 });
            assert_eq!(registry.lock().await.relay.is_some(), enable_relay);
        }
    }

    #[tokio::test]
    async fn node_lookup_ip_limit_survives_new_identity_and_connection_then_recovers() {
        let (addr, registry) = spawn_signal_server_with(SignalServerConfig {
            node_lookups_per_connection: 10,
            node_lookups_per_ip: 2,
            ..SignalServerConfig::for_tests()
        })
        .await;
        let admission = crate::relay::server::Admission::new(Default::default()).unwrap();
        registry.lock().await.relay = Some(admission.clone());
        let a = Identity::generate();
        let b = Identity::generate();
        let c = Identity::generate();
        let d = Identity::generate();
        let mut ra = Raw::connect(addr).await;
        let mut rb = Raw::connect(addr).await;
        let mut rc = Raw::connect(addr).await;
        ra.full_register(&a, vec![]).await;
        rb.full_register(&b, vec![]).await;
        rc.full_register(&c, vec![]).await;
        lookup_pair(&mut ra, a.node_id(), &mut rb, b.node_id()).await;
        lookup_pair(&mut rc, c.node_id(), &mut rb, b.node_id()).await;
        let mut reconnect = Raw::connect(addr).await;
        reconnect.full_register(&a, vec![]).await;
        let mut fresh_identity = Raw::connect(addr).await;
        fresh_identity.full_register(&d, vec![]).await;
        for raw in [&mut reconnect, &mut fresh_identity, &mut rc] {
            assert!(
                matches!(node_lookup_reply(raw, b.node_id()).await, SignalMessage::Error {reason} if reason == SHORT_LOOKUP_UNAVAILABLE)
            );
        }
        assert_eq!(admission.ticket_count(), 2);
        assert_target_has_no_candidates(&mut rb).await;
        tokio::time::pause();
        tokio::time::advance(NODE_LOOKUP_WINDOW).await;
        tokio::time::resume();
        lookup_pair(&mut fresh_identity, d.node_id(), &mut rb, b.node_id()).await;
        assert_eq!(admission.ticket_count(), 3);
    }

    #[tokio::test]
    async fn node_lookup_ip_buckets_are_bounded_and_expired_entries_are_reclaimed() {
        let config = SignalServerConfig {
            node_lookups_per_ip: 2,
            ..SignalServerConfig::for_tests()
        };
        let mut registry = Registry::default();
        for n in 0..MAX_NODE_LOOKUP_IPS {
            assert!(
                registry.take_node_lookup(IpAddr::V6(std::net::Ipv6Addr::from(n as u128)), &config)
            );
        }
        let first = IpAddr::V6(std::net::Ipv6Addr::UNSPECIFIED);
        assert!(registry.take_node_lookup(first, &config)); // Existing bucket still works at capacity.
        assert!(!registry.take_node_lookup(first, &config));
        let fresh = "192.0.2.1".parse().unwrap();
        assert!(!registry.take_node_lookup(fresh, &config));
        assert_eq!(registry.node_lookup_ips.len(), MAX_NODE_LOOKUP_IPS);
        assert!(registry.take_short_lookup(fresh, &config)); // Independent Short ID limiter.
        for bucket in registry.node_lookup_ips.values_mut() {
            bucket.started -= NODE_LOOKUP_WINDOW;
        }
        assert!(registry.take_node_lookup(fresh, &config));
        assert_eq!(registry.node_lookup_ips.len(), 1);
        assert!(registry.take_node_lookup(first, &config));
        assert_eq!(registry.node_lookup_ips.len(), 2);
    }

    #[tokio::test]
    async fn node_lookup_rate_limit_keeps_offline_waiter_until_registration_and_releases_pending_quota()
     {
        let (addr, registry) = spawn_signal_server_with(SignalServerConfig {
            node_lookups_per_connection: 2,
            max_pending_lookups: 1,
            ..SignalServerConfig::for_tests()
        })
        .await;
        let a = Identity::generate();
        let b = Identity::generate();
        let c = Identity::generate().node_id();
        let mut ra = Raw::connect(addr).await;
        ra.full_register(&a, vec![]).await;
        for _ in 0..2 {
            assert!(
                matches!(node_lookup_reply(&mut ra, b.node_id()).await, SignalMessage::PeerPending {node_id} if node_id == b.node_id())
            );
        }
        assert_eq!(registry.lock().await.waiter_entries(), 1);
        assert!(
            matches!(node_lookup_reply(&mut ra, c).await, SignalMessage::Error {reason} if reason == SHORT_LOOKUP_UNAVAILABLE)
        );
        assert_eq!(registry.lock().await.waiter_entries(), 1);
        let mut rb = Raw::connect(addr).await;
        rb.full_register(&b, vec![]).await;
        assert!(
            matches!(ra.recv().await, Some(SignalMessage::PeerCandidates {node_id, ..}) if node_id == b.node_id())
        );
        assert!(
            matches!(rb.recv().await, Some(SignalMessage::PeerCandidates {node_id, ..}) if node_id == a.node_id())
        );
        let reg = registry.lock().await;
        assert_eq!(reg.waiter_entries(), 0);
        assert!(reg.pending_by_connection.is_empty());
        drop(reg);
        tokio::time::pause();
        tokio::time::advance(NODE_LOOKUP_WINDOW).await;
        tokio::time::resume();
        assert!(
            matches!(node_lookup_reply(&mut ra, c).await, SignalMessage::PeerPending {node_id} if node_id == c)
        );
        assert_eq!(registry.lock().await.waiter_entries(), 1);
    }

    #[tokio::test]
    async fn relay_pairing_ticket_quota_does_not_starve_unrelated_real_node_lookups() {
        let (addr, registry) = spawn_signal_server_with(SignalServerConfig::for_tests()).await;
        let admission = crate::relay::server::Admission::new(Default::default()).unwrap();
        registry.lock().await.relay = Some(admission.clone());
        let a = Identity::generate();
        let b = Identity::generate();
        let c = Identity::generate();
        let d = Identity::generate();
        let mut ra = Raw::connect(addr).await;
        let mut rb = Raw::connect(addr).await;
        let mut rc = Raw::connect(addr).await;
        let mut rd = Raw::connect(addr).await;
        for (raw, id) in [(&mut ra, &a), (&mut rb, &b), (&mut rc, &c), (&mut rd, &d)] {
            raw.full_register(id, vec![]).await;
        }
        let quota = crate::relay::server::MAX_TICKETS_PER_PAIR;
        for _ in 0..quota + 4 {
            lookup_pair(&mut ra, a.node_id(), &mut rb, b.node_id()).await;
        }
        assert_eq!(admission.ticket_count(), quota);
        lookup_pair(&mut rc, c.node_id(), &mut rd, d.node_id()).await;
        assert_eq!(admission.ticket_count(), quota + 1);
    }

    #[test]
    fn ipv6_candidate_roundtrip_and_signature_cover_the_full_native_address() {
        let identity = Identity::generate();
        let challenge = [9_u8; CHALLENGE_LEN];
        let mut candidates = vec![
            Candidate::host("[fd00::1]:9000".parse().unwrap()),
            Candidate::host("[2001:db8::7]:9000".parse().unwrap()),
            Candidate::host("127.0.0.1:9000".parse().unwrap()),
        ];
        let payload = register_payload(
            &challenge,
            identity.node_id(),
            &identity.public_key_bytes(),
            &candidates,
        )
        .unwrap();
        let signature = identity.sign(&payload);
        let message = SignalMessage::Register {
            node_id: identity.node_id(),
            public_key: identity.public_key_bytes(),
            candidates: candidates.clone(),
            signature: signature.to_bytes().to_vec(),
        };
        assert_eq!(
            SignalMessage::decode(&message.encode().unwrap()).unwrap(),
            message
        );
        candidates[0].addr = "[fd00::2]:9000".parse().unwrap();
        let changed = register_payload(
            &challenge,
            identity.node_id(),
            &identity.public_key_bytes(),
            &candidates,
        )
        .unwrap();
        assert!(
            verify_signature(
                &public_key_from_bytes(&identity.public_key_bytes()).unwrap(),
                &changed,
                &signature
            )
            .is_err()
        );
        candidates.push(candidates[0]);
        dedup_candidates(&mut candidates);
        sort_candidates(&mut candidates);
        assert_eq!(candidates.len(), 3);
        assert!(candidates[0].addr.is_ipv6());
        assert!(candidates[1].addr.is_ipv6());
    }

    #[test]
    fn 候选排序_公网映射优先于中继() {
        let mut candidates = vec![
            candidate(CandidateKind::Relay, "10.0.0.1:1000"),
            candidate(CandidateKind::ServerReflexive, "203.0.113.5:5000"),
            candidate(CandidateKind::Host, "192.168.1.20:9000"),
        ];
        sort_candidates(&mut candidates);
        assert_eq!(candidates[0].kind, CandidateKind::ServerReflexive);
        assert_eq!(candidates[1].kind, CandidateKind::Host);
        assert_eq!(candidates[2].kind, CandidateKind::Relay);
    }

    #[test]
    fn 候选排序_同类里_ipv6_优先() {
        let mut candidates = vec![
            candidate(CandidateKind::Host, "192.168.1.20:9000"),
            candidate(CandidateKind::Host, "[2001:db8::20]:9000"),
        ];
        sort_candidates(&mut candidates);
        assert!(candidates[0].addr.is_ipv6());
    }

    #[test]
    fn 优先级数值单调() {
        assert!(CandidateKind::ServerReflexive.priority() < CandidateKind::Host.priority());
        assert!(CandidateKind::Host.priority() < CandidateKind::Predicted.priority());
        assert!(CandidateKind::Predicted.priority() < CandidateKind::Relay.priority());
    }

    #[test]
    fn 候选去重() {
        let mut candidates = vec![
            candidate(CandidateKind::Host, "192.168.1.20:9000"),
            candidate(CandidateKind::Host, "192.168.1.20:9000"),
            candidate(CandidateKind::ServerReflexive, "203.0.113.5:5000"),
            candidate(CandidateKind::Host, "192.168.1.21:9000"),
        ];
        dedup_candidates(&mut candidates);
        assert_eq!(candidates.len(), 3);
        // 同地址不同类型不算重复。
        candidates.push(candidate(CandidateKind::Predicted, "192.168.1.20:9000"));
        dedup_candidates(&mut candidates);
        assert_eq!(candidates.len(), 4);
    }

    #[test]
    fn 信令消息往返() {
        let node = NodeId::from_hex("00112233445566778899aabbccddeeff").unwrap();
        let messages = vec![
            SignalMessage::RegisterHello {
                protocol_version: SIGNAL_PROTOCOL_VERSION,
                node_id: node,
                public_key: [7u8; 32],
            },
            SignalMessage::RegisterChallenge {
                protocol_version: SIGNAL_PROTOCOL_VERSION,
                challenge: [5u8; CHALLENGE_LEN],
            },
            SignalMessage::Register {
                node_id: node,
                public_key: [7u8; 32],
                candidates: vec![candidate(CandidateKind::Host, "192.168.1.20:9000")],
                signature: vec![1u8; 64],
            },
            SignalMessage::Registered,
            SignalMessage::Lookup { node_id: node },
            SignalMessage::PeerCandidates {
                node_id: node,
                candidates: vec![],
                token: PunchToken::from_bytes([3u8; 16]),
            },
            SignalMessage::PeerPending { node_id: node },
            SignalMessage::Ping,
            SignalMessage::Pong,
            SignalMessage::Error {
                reason: "对端不在线".into(),
            },
        ];

        for message in messages {
            let decoded = SignalMessage::decode(&message.encode().unwrap()).unwrap();
            assert_eq!(message, decoded, "{} 往返失败", message.kind());
        }
    }

    #[test]
    fn 注册载荷绑定_challenge_与候选() {
        let alice = Identity::generate();
        let candidates = vec![candidate(CandidateKind::Host, "192.168.1.5:9000")];
        let base = register_payload(
            &[1u8; CHALLENGE_LEN],
            alice.node_id(),
            &alice.public_key_bytes(),
            &candidates,
        )
        .unwrap();

        // challenge 变了，载荷必须变（否则重放可行）。
        let other_challenge = register_payload(
            &[2u8; CHALLENGE_LEN],
            alice.node_id(),
            &alice.public_key_bytes(),
            &candidates,
        )
        .unwrap();
        assert_ne!(base, other_challenge);

        // 候选地址变了，载荷必须变（否则明文信令上的中间人可篡改地址）。
        let tampered = vec![candidate(CandidateKind::Host, "192.168.1.6:9000")];
        let other_candidates = register_payload(
            &[1u8; CHALLENGE_LEN],
            alice.node_id(),
            &alice.public_key_bytes(),
            &tampered,
        )
        .unwrap();
        assert_ne!(base, other_candidates, "候选地址必须进签名");
    }

    #[tokio::test]
    async fn 服务端能把双方牵到一起() {
        let addr = spawn_signal_server().await;

        let alice = Identity::generate();
        let bob = Identity::generate();
        let alice_id = alice.node_id();
        let bob_id = bob.node_id();

        let alice_candidates = vec![candidate(
            CandidateKind::ServerReflexive,
            "203.0.113.9:4000",
        )];
        let bob_candidates = vec![candidate(CandidateKind::Host, "192.168.1.7:9000")];

        let mut client_a =
            SignalingClient::connect(&addr.to_string(), &alice, alice_candidates.clone())
                .await
                .unwrap();

        // Bob 还没上线，Alice 查他应当是 Pending。
        assert_eq!(
            client_a.lookup(bob_id).await.unwrap(),
            LookupOutcome::Pending
        );

        let mut client_b =
            SignalingClient::connect(&addr.to_string(), &bob, bob_candidates.clone())
                .await
                .unwrap();

        // Bob 上线后，Alice 应当收到推送。
        let got = client_a
            .wait_for_peer(bob_id, Duration::from_secs(5))
            .await
            .unwrap();
        assert_eq!(
            got.unwrap().candidates,
            bob_candidates,
            "Alice 应拿到 Bob 的候选"
        );

        // Bob 那边也应当被推送了 Alice 的候选。
        let got = client_b
            .wait_for_peer(alice_id, Duration::from_secs(5))
            .await
            .unwrap();
        assert_eq!(
            got.unwrap().candidates,
            alice_candidates,
            "Bob 应拿到 Alice 的候选"
        );
    }

    #[tokio::test]
    async fn 同一对节点拿到的令牌必须一致() {
        // 令牌是打洞时「接受任意来源探测包」的凭据。两边要是拿到不同的令牌，
        // 谁也认不下谁的探测包，表现为「明明都在线却怎么都打不通」。
        let addr = spawn_signal_server().await;

        let alice = Identity::generate();
        let bob = Identity::generate();
        let bob_id = bob.node_id();

        let mut client_a = SignalingClient::connect(
            &addr.to_string(),
            &alice,
            vec![candidate(CandidateKind::Host, "192.168.1.1:9000")],
        )
        .await
        .unwrap();

        // Alice 先挂起等待，Bob 一上线服务器就会给两人推同一份令牌。
        assert_eq!(
            client_a.lookup(bob_id).await.unwrap(),
            LookupOutcome::Pending
        );

        let mut client_b = SignalingClient::connect(
            &addr.to_string(),
            &bob,
            vec![candidate(CandidateKind::Host, "192.168.1.2:9000")],
        )
        .await
        .unwrap();

        let alice_side = client_a
            .wait_for_peer(bob_id, Duration::from_secs(5))
            .await
            .unwrap()
            .expect("Alice 应当收到推送");

        // Bob 的候选是服务器在他登记时推给他的，直接取即可。
        let bob_side = client_b
            .wait_for_peer(alice.node_id(), Duration::from_secs(5))
            .await
            .unwrap()
            .expect("Bob 也应当收到推送");

        assert_eq!(
            alice_side.token, bob_side.token,
            "同一次牵线推给双方的令牌必须一致"
        );
        assert_eq!(
            alice_side.candidates,
            vec![candidate(CandidateKind::Host, "192.168.1.2:9000")]
        );
        assert_eq!(
            bob_side.candidates,
            vec![candidate(CandidateKind::Host, "192.168.1.1:9000")]
        );
    }

    #[tokio::test]
    async fn 对端被强杀后会从在线表里摘掉() {
        // 踩过的坑：对端进程被 kill -9 时，服务器读到的是 RST（返回 Err）而不是
        // 干净的 EOF（返回 Ok(None)）。原先读循环用 `?` 直接返回，跳过了下线
        // 清理，于是死掉的节点一直「在线」，服务器会把过期候选地址发给别人，
        // 对端照着打洞只能打到空气上。
        //
        // 这里用裸 TcpStream 而不是 SignalingClient：`into_split` 出来的两个半边
        // 共享同一个 fd，只 drop 一半不会真正关闭连接，也就发不出 RST。
        let addr = spawn_signal_server().await;

        let alice = Identity::generate();
        let bob = Identity::generate();
        let bob_id = bob.node_id();

        let mut client_a = SignalingClient::connect(
            &addr.to_string(),
            &alice,
            vec![candidate(CandidateKind::Host, "192.168.1.1:9000")],
        )
        .await
        .unwrap();

        // Bob 用一个裸连接登记，登记完立刻带 RST 消失。
        let mut raw = Raw::connect(addr).await;
        raw.full_register(
            &bob,
            vec![candidate(CandidateKind::Host, "192.168.1.2:9000")],
        )
        .await;

        // Bob 确实在线。
        assert!(
            matches!(
                client_a.lookup(bob_id).await.unwrap(),
                LookupOutcome::Ready(_)
            ),
            "Bob 刚登记完，应当是在线的"
        );

        // SO_LINGER=0：close 时不走四次挥手，直接发 RST。
        // 这个方法在 tokio 里被标了 deprecated（担心阻塞 drop），测试里用没问题。
        #[allow(deprecated)]
        raw.stream.set_linger(Some(Duration::ZERO)).unwrap();
        drop(raw);

        // 服务器必须察觉并摘掉它。
        let mut gone = false;
        for _ in 0..50 {
            tokio::time::sleep(Duration::from_millis(100)).await;
            if matches!(
                client_a.lookup(bob_id).await.unwrap(),
                LookupOutcome::Pending
            ) {
                gone = true;
                break;
            }
        }
        assert!(gone, "对端已经被强杀，服务器却还认为它在线");
    }

    #[tokio::test]
    async fn 双方同时上线也能牵到一起() {
        let addr = spawn_signal_server().await;
        let alice = Identity::generate();
        let bob = Identity::generate();

        let mut client_a = SignalingClient::connect(
            &addr.to_string(),
            &alice,
            vec![candidate(CandidateKind::Host, "192.168.1.1:9000")],
        )
        .await
        .unwrap();
        let mut client_b = SignalingClient::connect(
            &addr.to_string(),
            &bob,
            vec![candidate(CandidateKind::Host, "192.168.1.2:9000")],
        )
        .await
        .unwrap();

        // 两边同时查对方。
        let (a, b) = tokio::join!(
            client_a.resolve_peer(bob.node_id(), Duration::from_secs(5)),
            client_b.resolve_peer(alice.node_id(), Duration::from_secs(5)),
        );
        assert_eq!(a.unwrap().candidates.len(), 1);
        assert_eq!(b.unwrap().candidates.len(), 1);
    }

    #[tokio::test]
    async fn 查自己会被拒绝() {
        let addr = spawn_signal_server().await;
        let alice = Identity::generate();
        let mut client = SignalingClient::connect(&addr.to_string(), &alice, vec![])
            .await
            .unwrap();

        let err = client.lookup(alice.node_id()).await.unwrap_err();
        assert!(matches!(err, Error::Discovery(_)), "实际 {err:?}");
    }

    #[tokio::test]
    async fn 连不上信令服务器会报错() {
        // 占一个端口再释放，保证拿到一个没人监听的地址。
        let dead = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = dead.local_addr().unwrap();
        drop(dead);

        let alice = Identity::generate();
        let err = SignalingClient::connect(&addr.to_string(), &alice, vec![])
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Discovery(_)), "实际 {err:?}");
    }

    #[tokio::test]
    async fn 等对端超时会报错() {
        let addr = spawn_signal_server().await;
        let alice = Identity::generate();
        let ghost = Identity::generate().node_id();
        let mut client = SignalingClient::connect(&addr.to_string(), &alice, vec![])
            .await
            .unwrap();

        let err = client
            .resolve_peer(ghost, Duration::from_millis(300))
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Discovery(_)), "实际 {err:?}");
    }

    #[tokio::test]
    async fn 心跳之后连接仍然可用() {
        let addr = spawn_signal_server().await;
        let alice = Identity::generate();
        let mut client = SignalingClient::connect(&addr.to_string(), &alice, vec![])
            .await
            .unwrap();

        assert_eq!(client.node_id(), alice.node_id());
        assert_eq!(client.server(), addr.to_string());
        client.ping().await.unwrap();

        let ghost = Identity::generate().node_id();
        assert_eq!(client.lookup(ghost).await.unwrap(), LookupOutcome::Pending);
    }

    #[tokio::test]
    async fn 单事件接口保留心跳与查询响应供同一分发器处理() {
        let addr = spawn_signal_server().await;
        let alice = Identity::generate();
        let bob = Identity::generate();
        let carol = Identity::generate();
        let mut client_alice =
            SignalingClient::connect_with_events(&addr.to_string(), &alice, vec![])
                .await
                .unwrap();
        let mut client_bob = SignalingClient::connect_with_events(&addr.to_string(), &bob, vec![])
            .await
            .unwrap();
        let client_carol = SignalingClient::connect(&addr.to_string(), &carol, vec![])
            .await
            .unwrap();

        // Bob actively looks up Alice while Carol independently offers Bob as a
        // passive target. One dispatcher must retain both peer events.
        client_bob.request_lookup(alice.node_id()).await.unwrap();
        client_carol.request_lookup(bob.node_id()).await.unwrap();
        client_bob.ping().await.unwrap();

        let mut bob_peers = HashSet::new();
        let mut got_pong = false;
        for _ in 0..4 {
            let event = tokio::time::timeout(Duration::from_secs(3), client_bob.next_event())
                .await
                .expect("同一 reader 应持续路由所有事件")
                .unwrap();
            match event {
                SignalMessage::PeerCandidates { node_id, .. } => {
                    bob_peers.insert(node_id);
                }
                SignalMessage::Pong => got_pong = true,
                other => panic!("意外事件：{other:?}"),
            }
            if bob_peers.contains(&alice.node_id())
                && bob_peers.contains(&carol.node_id())
                && got_pong
            {
                break;
            }
        }
        assert!(
            bob_peers.contains(&alice.node_id()),
            "主动查询 A 的响应不能丢"
        );
        assert!(
            bob_peers.contains(&carol.node_id()),
            "主动查询 C 的响应不能丢"
        );
        assert!(got_pong, "lookup 之间的 Pong 不能被吞");

        let passive = tokio::time::timeout(Duration::from_secs(3), client_alice.next_event())
            .await
            .expect("被动目标 A 应收到 Bob 的 offer")
            .unwrap();
        assert!(
            matches!(passive, SignalMessage::PeerCandidates { node_id, .. } if node_id == bob.node_id())
        );
    }

    #[tokio::test]
    async fn 同一节点重连会覆盖旧候选() {
        let addr = spawn_signal_server().await;
        let alice = Identity::generate();
        let bob = Identity::generate();

        let mut client_b = SignalingClient::connect(
            &addr.to_string(),
            &bob,
            vec![candidate(CandidateKind::Host, "192.168.1.7:9000")],
        )
        .await
        .unwrap();

        let mut client_a = SignalingClient::connect(
            &addr.to_string(),
            &alice,
            vec![candidate(CandidateKind::Host, "192.168.1.1:9000")],
        )
        .await
        .unwrap();

        // Bob 换了个端口重连。
        let mut client_b2 = SignalingClient::connect(
            &addr.to_string(),
            &bob,
            vec![candidate(CandidateKind::Host, "192.168.1.7:9100")],
        )
        .await
        .unwrap();

        let got = client_a
            .resolve_peer(bob.node_id(), Duration::from_secs(5))
            .await
            .unwrap();
        assert_eq!(
            got.candidates,
            vec![candidate(CandidateKind::Host, "192.168.1.7:9100")],
            "应当拿到重连后的新候选"
        );

        // 旧连接必须被服务器**主动关闭**：否则拿同一个私钥不停重连，旧连接靠
        // Ping 就能无限续命，socket/task 无限堆积（`peers` 只算一条，
        // `max_registered_peers` 完全挡不住）。
        let ghost = Identity::generate().node_id();
        let closed = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if client_b.lookup(ghost).await.is_err() {
                    break;
                }
            }
        })
        .await;
        assert!(closed.is_ok(), "被取代的旧连接应当被服务器主动关闭");

        // 新连接继续可用。
        assert_eq!(
            client_b2.lookup(ghost).await.unwrap(),
            LookupOutcome::Pending
        );
    }

    // -----------------------------------------------------------------------
    // Issue #2：注册认证
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn 节点_id_与公钥不匹配时注册被拒绝() {
        let (addr, registry) = spawn_signal_server_with(SignalServerConfig::for_tests()).await;
        let alice = Identity::generate();
        let mallory = Identity::generate();

        let mut raw = Raw::connect(addr).await;
        raw.send(&SignalMessage::RegisterHello {
            protocol_version: SIGNAL_PROTOCOL_VERSION,
            node_id: alice.node_id(),
            public_key: mallory.public_key_bytes(),
        })
        .await;

        match raw.recv().await {
            Some(SignalMessage::Error { reason }) => {
                assert!(reason.contains("公钥"), "实际: {reason}");
            }
            other => panic!("公钥与节点 ID 不符时必须拒绝，实际 {other:?}"),
        }
        assert!(registry.lock().await.peers.is_empty(), "不能写进在线表");
        // 服务端应当关闭连接。
        assert!(
            tokio::time::timeout(Duration::from_secs(2), raw.recv())
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn 签名不是对应私钥时注册被拒绝() {
        let (addr, registry) = spawn_signal_server_with(SignalServerConfig::for_tests()).await;
        let mallory = Identity::generate();
        let impostor = Identity::generate();

        let mut raw = Raw::connect(addr).await;
        let challenge = raw.hello(&mallory).await;
        let reply = raw
            .register_with(
                &mallory,
                &impostor,
                &challenge,
                vec![candidate(CandidateKind::Host, "192.168.1.9:9000")],
            )
            .await;

        match reply {
            Some(SignalMessage::Error { reason }) => {
                assert!(reason.contains("签名"), "实际: {reason}");
            }
            other => panic!("签名不匹配私钥时必须拒绝，实际 {other:?}"),
        }
        assert!(
            registry.lock().await.peers.is_empty(),
            "验签失败绝对不能写进在线表"
        );
    }

    #[tokio::test]
    async fn 重放上一连接的注册签名会被拒绝() {
        let (addr, registry) = spawn_signal_server_with(SignalServerConfig::for_tests()).await;
        let bob = Identity::generate();
        let candidates = vec![candidate(CandidateKind::Host, "192.168.1.7:9000")];

        // 第一条连接合法注册，留下 (challenge1, signature1)。
        let mut first = Raw::connect(addr).await;
        let challenge1 = first.hello(&bob).await;
        let payload1 = register_payload(
            &challenge1,
            bob.node_id(),
            &bob.public_key_bytes(),
            &candidates,
        )
        .unwrap();
        let replay = SignalMessage::Register {
            node_id: bob.node_id(),
            public_key: bob.public_key_bytes(),
            candidates: candidates.clone(),
            signature: bob.sign(&payload1).to_bytes().to_vec(),
        };
        first.send(&replay).await;
        assert!(matches!(
            first.recv().await,
            Some(SignalMessage::Registered)
        ));

        // 第二条连接会拿到一个新 challenge，把旧 Register 原样搬过去必须失败。
        let mut second = Raw::connect(addr).await;
        let challenge2 = second.hello(&bob).await;
        assert_ne!(challenge1, challenge2, "每条连接必须是新随机 challenge");
        second.send(&replay).await;
        match second.recv().await {
            Some(SignalMessage::Error { reason }) => {
                assert!(reason.contains("签名"), "实际: {reason}");
            }
            other => panic!("重放必须失败，实际 {other:?}"),
        }

        // 第一条连接不受影响，仍然是当前记录。
        assert!(registry.lock().await.peers.contains_key(&bob.node_id()));
    }

    #[tokio::test]
    async fn 篡改候选地址会导致签名校验失败() {
        let (addr, registry) = spawn_signal_server_with(SignalServerConfig::for_tests()).await;
        let alice = Identity::generate();

        let mut raw = Raw::connect(addr).await;
        let challenge = raw.hello(&alice).await;
        // 签名覆盖的是这些候选……
        let signed = vec![candidate(CandidateKind::Host, "192.168.1.5:9000")];
        let payload = register_payload(
            &challenge,
            alice.node_id(),
            &alice.public_key_bytes(),
            &signed,
        )
        .unwrap();
        // ……但发出去的是被篡改过的候选。
        let tampered = vec![candidate(CandidateKind::Host, "203.0.113.66:9000")];
        raw.send(&SignalMessage::Register {
            node_id: alice.node_id(),
            public_key: alice.public_key_bytes(),
            candidates: tampered,
            signature: alice.sign(&payload).to_bytes().to_vec(),
        })
        .await;

        assert!(matches!(
            raw.recv().await,
            Some(SignalMessage::Error { .. })
        ));
        assert!(registry.lock().await.peers.is_empty());
    }

    #[tokio::test]
    async fn 同一连接不能重复注册或切换身份() {
        let (addr, registry) = spawn_signal_server_with(SignalServerConfig::for_tests()).await;
        let alice = Identity::generate();
        let bob = Identity::generate();

        let mut raw = Raw::connect(addr).await;
        raw.full_register(
            &alice,
            vec![candidate(CandidateKind::Host, "192.168.1.1:9000")],
        )
        .await;

        // 想在同一条连接上再注册另一个身份（旧代码会留下幽灵记录）。
        raw.send(&SignalMessage::RegisterHello {
            protocol_version: SIGNAL_PROTOCOL_VERSION,
            node_id: bob.node_id(),
            public_key: bob.public_key_bytes(),
        })
        .await;
        match raw.recv().await {
            Some(SignalMessage::Error { reason }) => {
                assert!(reason.contains("注册"), "实际: {reason}");
            }
            other => panic!("重复注册必须被拒绝，实际 {other:?}"),
        }

        // 连接被关闭；registry 里不能有 bob，alice 也随连接断开被清掉。
        assert!(
            tokio::time::timeout(Duration::from_secs(2), raw.recv())
                .await
                .unwrap()
                .is_none()
        );
        wait_for_registry(&registry, |reg| reg.peers.is_empty()).await;
        let reg = registry.lock().await;
        assert!(!reg.peers.contains_key(&bob.node_id()), "不能留下幽灵节点");
        assert!(reg.peers.is_empty());
    }

    #[tokio::test]
    async fn 旧连接断开不会删除新连接() {
        let (addr, registry) = spawn_signal_server_with(SignalServerConfig::for_tests()).await;
        let alice = Identity::generate();
        let old_candidates = vec![candidate(CandidateKind::Host, "192.168.1.1:9000")];
        let new_candidates = vec![candidate(CandidateKind::Host, "192.168.1.1:9100")];

        let mut old = Raw::connect(addr).await;
        old.full_register(&alice, old_candidates).await;

        let mut new = Raw::connect(addr).await;
        new.full_register(&alice, new_candidates.clone()).await;

        {
            let reg = registry.lock().await;
            let record = reg.peers.get(&alice.node_id()).expect("alice 应当在线");
            assert_eq!(record.candidates, new_candidates);
        }

        // 旧连接断开，它的清理不能删掉新连接的记录。
        drop(old);
        tokio::time::sleep(Duration::from_millis(300)).await;
        {
            let reg = registry.lock().await;
            let record = reg.peers.get(&alice.node_id()).expect("新连接必须仍然在线");
            assert_eq!(record.candidates, new_candidates);
        }

        // 新连接依然可用。
        new.send(&SignalMessage::Ping).await;
        assert!(matches!(new.recv().await, Some(SignalMessage::Pong)));
    }

    #[tokio::test]
    async fn 旧连接注销不会删除新连接的记录() {
        // 直接对 Registry 做确定性验证：connection_id 所有权。
        let (outbox_old, _rx_old) = mpsc::channel(1);
        let (close_old, _) = watch::channel(false);
        let (outbox_new, _rx_new) = mpsc::channel(1);
        let (close_new, _) = watch::channel(false);

        let alice = Identity::generate().node_id();
        let mut reg = Registry::default();
        reg.peers.insert(
            alice,
            PeerRecord {
                connection_id: 1,
                candidates: vec![],
                outbox: outbox_old,
                close: close_old,
            },
        );
        reg.peers.insert(
            alice,
            PeerRecord {
                connection_id: 2,
                candidates: vec![],
                outbox: outbox_new,
                close: close_new,
            },
        );

        // 旧连接（generation 1）的清理不得删除 generation 2 的记录。
        reg.unregister(alice, 1);
        assert_eq!(
            reg.peers.get(&alice).map(|record| record.connection_id),
            Some(2)
        );

        // 新连接断开时才真正摘掉。
        reg.unregister(alice, 2);
        assert!(!reg.peers.contains_key(&alice));
    }

    #[test]
    fn 断开后空_waiter_会被清理() {
        let requester = Identity::generate().node_id();
        let other = Identity::generate().node_id();
        let mut reg = Registry::default();

        let key_requester = WaiterKey {
            requester,
            connection_id: 1,
        };
        for _ in 0..8 {
            let target = Identity::generate().node_id();
            assert!(reg.add_waiter(key_requester, target));
        }
        // 另一个 requester 的等待不该被误删。
        let kept_target = Identity::generate().node_id();
        let key_other = WaiterKey {
            requester: other,
            connection_id: 9,
        };
        assert!(reg.add_waiter(key_other, kept_target));
        // 一个从一开始就空的集合。
        reg.waiters
            .entry(Identity::generate().node_id())
            .or_default();

        reg.unregister(requester, 1);

        assert!(
            reg.waiters.values().all(|waiters| !waiters.is_empty()),
            "不该留下空 set"
        );
        assert_eq!(reg.waiter_entries(), 1, "只应剩下 other 的那一条");
        assert!(reg.waiters.get(&kept_target).unwrap().contains(&key_other));
        assert_eq!(reg.pending_count(key_requester), 0);
        assert_eq!(reg.pending_count(key_other), 1);
    }

    #[test]
    fn 旧连接的清理不会删除新连接的_waiter() {
        // A(old) 和 A(new) 都在等不同的目标；A(old) 断开时只能清掉自己那条。
        let alice = Identity::generate().node_id();
        let old_key = WaiterKey {
            requester: alice,
            connection_id: 1,
        };
        let new_key = WaiterKey {
            requester: alice,
            connection_id: 2,
        };
        let old_target = Identity::generate().node_id();
        let new_target = Identity::generate().node_id();

        let mut reg = Registry::default();
        reg.peers.insert(
            alice,
            PeerRecord {
                connection_id: 2,
                candidates: vec![],
                outbox: mpsc::channel(1).0,
                close: watch::channel(false).0,
            },
        );
        assert!(reg.add_waiter(old_key, old_target));
        assert!(reg.add_waiter(new_key, new_target));

        // 旧连接断开。
        reg.unregister(alice, 1);

        assert!(
            !reg.waiters.contains_key(&old_target) || reg.waiters[&old_target].is_empty(),
            "旧连接自己的 waiter 应当被清掉"
        );
        assert_eq!(
            reg.waiters.get(&new_target).map(HashSet::len),
            Some(1),
            "新连接创建的 waiter 不能被旧连接的清理删掉"
        );
        assert_eq!(reg.pending_count(new_key), 1);
        assert_eq!(reg.pending_count(old_key), 0);
    }

    #[test]
    fn 旧连接的_waiter_不会被新连接继承() {
        // A(old) 挂起等 X；A 随后以同 NodeId 重连（覆盖 peers[A]）。
        // 之后 X 上线，不能把 X 推给从未重新发起查询的新连接。
        let alice = Identity::generate().node_id();
        let x = Identity::generate().node_id();
        let old_key = WaiterKey {
            requester: alice,
            connection_id: 1,
        };

        let mut reg = Registry::default();
        reg.peers.insert(
            alice,
            PeerRecord {
                connection_id: 1,
                candidates: vec![],
                outbox: mpsc::channel(1).0,
                close: watch::channel(false).0,
            },
        );
        assert!(reg.add_waiter(old_key, x));

        // A(new) 重连，覆盖当前记录。
        reg.peers.insert(
            alice,
            PeerRecord {
                connection_id: 2,
                candidates: vec![],
                outbox: mpsc::channel(1).0,
                close: watch::channel(false).0,
            },
        );

        // X 上线：旧 waiter 的 connection_id 已经不是当前连接，必须被丢掉。
        let live = reg.take_waiters_for(x);
        assert!(
            live.is_empty(),
            "旧连接的 waiter 不能被新连接继承，实际 {live:?}"
        );
        assert_eq!(reg.pending_count(old_key), 0, "配额必须一起释放");
        assert_eq!(reg.waiter_entries(), 0);
    }

    #[test]
    fn waiter_被消费后挂起配额会释放() {
        let alice = Identity::generate().node_id();
        let x = Identity::generate().node_id();
        let key = WaiterKey {
            requester: alice,
            connection_id: 1,
        };

        let mut reg = Registry::default();
        reg.peers.insert(
            alice,
            PeerRecord {
                connection_id: 1,
                candidates: vec![],
                outbox: mpsc::channel(1).0,
                close: watch::channel(false).0,
            },
        );
        assert!(reg.add_waiter(key, x));
        assert_eq!(reg.pending_count(key), 1);

        // X 上线，waiter 被消费。
        let live = reg.take_waiters_for(x);
        assert_eq!(live, vec![alice]);
        assert_eq!(reg.pending_count(key), 0, "配对完成后配额必须释放");
        assert_eq!(reg.waiter_entries(), 0);

        // 同一个 key 可以马上再挂新的等待，不会被历史记录挤掉。
        let y = Identity::generate().node_id();
        assert!(reg.add_waiter(key, y));
        assert_eq!(reg.pending_count(key), 1);
    }

    #[tokio::test]
    async fn lookup_会去重且每连接有上限() {
        let config = SignalServerConfig {
            max_pending_lookups: 2,
            ..SignalServerConfig::for_tests()
        };
        let (addr, registry) = spawn_signal_server_with(config).await;
        let alice = Identity::generate();
        let mut client = SignalingClient::connect(&addr.to_string(), &alice, vec![])
            .await
            .unwrap();

        let first = Identity::generate().node_id();
        let second = Identity::generate().node_id();
        let third = Identity::generate().node_id();

        assert_eq!(client.lookup(first).await.unwrap(), LookupOutcome::Pending);
        // 重复查询同一个目标不该重复计数。
        assert_eq!(client.lookup(first).await.unwrap(), LookupOutcome::Pending);
        assert_eq!(
            registry.lock().await.waiter_entries(),
            1,
            "重复 lookup 必须去重"
        );

        assert_eq!(client.lookup(second).await.unwrap(), LookupOutcome::Pending);
        assert_eq!(registry.lock().await.waiter_entries(), 2);

        // 超过单连接上限：明确报错，且不写入。
        let err = client.lookup(third).await.unwrap_err();
        assert!(matches!(err, Error::Discovery(_)), "实际 {err:?}");
        assert_eq!(
            registry.lock().await.waiter_entries(),
            2,
            "超限的查询不得写入"
        );
    }

    #[tokio::test]
    async fn 候选地址超限会被拒绝() {
        let (addr, registry) = spawn_signal_server_with(SignalServerConfig::for_tests()).await;
        let alice = Identity::generate();
        let mut raw = Raw::connect(addr).await;
        let challenge = raw.hello(&alice).await;

        let mut candidates = Vec::new();
        for index in 0..(MAX_CANDIDATES + 1) {
            let addr = format!("192.168.1.{}:{}", (index % 250) + 1, 9000 + index);
            candidates.push(candidate(CandidateKind::Host, &addr));
        }
        let reply = raw
            .register_with(&alice, &alice, &challenge, candidates)
            .await;

        match reply {
            Some(SignalMessage::Error { reason }) => {
                assert!(reason.contains("候选"), "实际: {reason}");
            }
            other => panic!("候选超限必须拒绝，实际 {other:?}"),
        }
        assert!(registry.lock().await.peers.is_empty());
    }

    #[tokio::test]
    async fn 信令帧超限会被拒绝并断开() {
        let addr = spawn_signal_server().await;
        let mut raw = Raw::connect(addr).await;
        // 只写一个超过信令上限的长度头，服务端必须在分配前拒绝并断开。
        raw.send_len_header(MAX_SIGNAL_FRAME_LEN + 1).await;

        let closed = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if raw.recv().await.is_none() {
                    break true;
                }
            }
        })
        .await;
        assert!(closed.unwrap(), "帧超限时必须关闭连接");
    }

    #[tokio::test]
    async fn outbox_满时牵线会关闭异常连接而不是只发一边() {
        // 有界队列 + try_reserve 的意义：排不下就断开，而不是无限积压，
        // 也不是把令牌只发给一边（那会让双方拿到不同令牌）。
        let (outbox_a, mut rx_a) = mpsc::channel(1);
        let (close_a, _close_a_rx) = watch::channel(false);
        let (outbox_b, _rx_b) = mpsc::channel(1);
        let (close_b, close_b_rx) = watch::channel(false);

        let a = Identity::generate().node_id();
        let b = Identity::generate().node_id();

        // 把 B 的队列占满。
        outbox_b.try_send(SignalMessage::Pong).unwrap();

        let mut reg = Registry::default();
        reg.peers.insert(
            a,
            PeerRecord {
                connection_id: 1,
                candidates: vec![],
                outbox: outbox_a,
                close: close_a,
            },
        );
        reg.peers.insert(
            b,
            PeerRecord {
                connection_id: 2,
                candidates: vec![],
                outbox: outbox_b,
                close: close_b,
            },
        );

        reg.try_pair(a, b, MAX_CANDIDATES);

        assert!(
            close_b_rx.has_changed().unwrap_or(false),
            "队列已满的连接应当被要求断开"
        );
        assert!(rx_a.try_recv().is_err(), "牵线失败时不能只把令牌发给一边");
    }

    #[tokio::test]
    async fn 首帧超时会关闭连接() {
        let config = SignalServerConfig {
            first_frame_timeout: Duration::from_millis(150),
            ..SignalServerConfig::for_tests()
        };
        let (addr, _) = spawn_signal_server_with(config).await;
        let mut raw = Raw::connect(addr).await;

        let closed = tokio::time::timeout(Duration::from_secs(3), raw.recv())
            .await
            .expect("不该挂死");
        assert!(closed.is_none(), "首帧超时后应关闭连接");
    }

    #[tokio::test]
    async fn 注册应答超时会关闭连接() {
        let config = SignalServerConfig {
            register_timeout: Duration::from_millis(150),
            ..SignalServerConfig::for_tests()
        };
        let (addr, registry) = spawn_signal_server_with(config).await;
        let alice = Identity::generate();
        let mut raw = Raw::connect(addr).await;
        let _ = raw.hello(&alice).await; // 拿到 challenge 后故意不发 Register

        let closed = tokio::time::timeout(Duration::from_secs(3), raw.recv())
            .await
            .expect("不该挂死");
        assert!(closed.is_none(), "注册超时后应关闭连接");
        assert!(registry.lock().await.peers.is_empty());
    }

    #[tokio::test]
    async fn 登记后长时间空闲会被断开() {
        let config = SignalServerConfig {
            idle_timeout: Duration::from_millis(150),
            ..SignalServerConfig::for_tests()
        };
        let (addr, registry) = spawn_signal_server_with(config).await;
        let alice = Identity::generate();
        let mut raw = Raw::connect(addr).await;
        raw.full_register(&alice, vec![]).await;

        let closed = tokio::time::timeout(Duration::from_secs(3), raw.recv())
            .await
            .expect("不该挂死");
        assert!(closed.is_none(), "空闲超时后应关闭连接");
        wait_for_registry(&registry, |reg| reg.peers.is_empty()).await;
    }

    #[tokio::test]
    async fn 同_node_id_重连后旧连接被主动关闭() {
        // 只靠「旧连接不能再 Lookup」是不够的：旧连接还能 Ping，就能靠心跳
        // 无限续命，socket/task 无限堆积。服务器必须在登记新连接时主动踢掉旧的。
        let (addr, registry) = spawn_signal_server_with(SignalServerConfig::for_tests()).await;
        let alice = Identity::generate();

        let mut old = Raw::connect(addr).await;
        old.full_register(
            &alice,
            vec![candidate(CandidateKind::Host, "192.168.1.1:9000")],
        )
        .await;

        let mut new = Raw::connect(addr).await;
        new.full_register(
            &alice,
            vec![candidate(CandidateKind::Host, "192.168.1.1:9100")],
        )
        .await;

        // 旧连接必须收到 EOF（服务器没再给它发任何东西，所以 None 就是被关了）。
        let closed = tokio::time::timeout(Duration::from_secs(3), old.recv())
            .await
            .expect("不该挂死");
        assert!(
            closed.is_none(),
            "旧连接必须被服务器主动关闭，实际收到 {closed:?}"
        );

        // 在线表里是新连接的记录，新连接依然可用。
        {
            let reg = registry.lock().await;
            let record = reg.peers.get(&alice.node_id()).expect("新连接应当在线");
            assert_eq!(
                record.candidates,
                vec![candidate(CandidateKind::Host, "192.168.1.1:9100")]
            );
        }
        new.send(&SignalMessage::Ping).await;
        assert!(matches!(new.recv().await, Some(SignalMessage::Pong)));
    }

    #[tokio::test]
    async fn 自动心跳让长连接跨过多个空闲周期仍在线() {
        // idle 给得比心跳周期宽裕（4 倍），避免并行跑测试时调度抖动导致误判。
        let idle = Duration::from_millis(400);
        let config = SignalServerConfig {
            idle_timeout: idle,
            ..SignalServerConfig::for_tests()
        };
        let (addr, registry) = spawn_signal_server_with(config).await;
        let alice = Identity::generate();

        // 心跳周期明显小于 idle_timeout。
        let interval = Duration::from_millis(100);
        let mut client = SignalingClient::connect_with_heartbeat(
            &addr.to_string(),
            &alice,
            vec![],
            Some(interval),
        )
        .await
        .unwrap();

        // 期间上层完全不碰信令，睡过好几个 idle_timeout 周期。
        let mut elapsed = Duration::ZERO;
        while elapsed < idle * 5 {
            tokio::time::sleep(interval).await;
            elapsed += interval;
            assert!(
                registry.lock().await.peers.contains_key(&alice.node_id()),
                "自动心跳期间必须保持在线（已过 {elapsed:?}）"
            );
        }

        // 连接仍然可用。
        let ghost = Identity::generate().node_id();
        assert_eq!(client.lookup(ghost).await.unwrap(), LookupOutcome::Pending);
    }

    #[tokio::test]
    async fn 停止心跳后会被服务器清理() {
        // 关掉自动心跳：只有注册，之后再没有任何帧。
        // 证明「保持在线」靠的确实是心跳，而不是别的什么。
        let idle = Duration::from_millis(300);
        let config = SignalServerConfig {
            idle_timeout: idle,
            ..SignalServerConfig::for_tests()
        };
        let (addr, registry) = spawn_signal_server_with(config).await;
        let alice = Identity::generate();

        let mut client =
            SignalingClient::connect_with_heartbeat(&addr.to_string(), &alice, vec![], None)
                .await
                .unwrap();
        assert!(
            registry.lock().await.peers.contains_key(&alice.node_id()),
            "刚登记时应当在线"
        );

        // 没有任何心跳，服务器最终按空闲超时把它摘掉。
        wait_for_registry(&registry, |reg| !reg.peers.contains_key(&alice.node_id())).await;

        // 连接也会被服务器关掉。
        let ghost = Identity::generate().node_id();
        let closed = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                if client.lookup(ghost).await.is_err() {
                    break;
                }
            }
        })
        .await;
        assert!(closed.is_ok(), "停掉心跳后连接最终应被服务器关闭");
    }

    #[tokio::test]
    async fn waiter_完成后挂起配额会释放() {
        // 配额必须反映「当前真实挂起」，不能把已经配对完成的历史目标也算进去。
        let config = SignalServerConfig {
            max_pending_lookups: 1,
            ..SignalServerConfig::for_tests()
        };
        let (addr, _registry) = spawn_signal_server_with(config).await;
        let alice = Identity::generate();
        let mut client = SignalingClient::connect(&addr.to_string(), &alice, vec![])
            .await
            .unwrap();

        let target = Identity::generate();
        let blocked = Identity::generate();

        // 用满唯一一个配额。
        assert_eq!(
            client.lookup(target.node_id()).await.unwrap(),
            LookupOutcome::Pending
        );
        assert!(
            client.lookup(blocked.node_id()).await.is_err(),
            "配额用满时新查询应当被拒"
        );

        // target 上线 → waiter 被消费 → 配额释放。
        let mut target_client = SignalingClient::connect(&addr.to_string(), &target, vec![])
            .await
            .unwrap();
        let offer = client
            .wait_for_peer(target.node_id(), Duration::from_secs(3))
            .await
            .unwrap();
        assert!(offer.is_some(), "对方上线后应当收到推送");

        // 现在可以继续查询新目标。
        assert_eq!(
            client.lookup(blocked.node_id()).await.unwrap(),
            LookupOutcome::Pending,
            "已完成的等待必须释放配额"
        );

        // target 侧也能正常查询，连接没被前一步影响。
        assert_eq!(
            target_client.lookup(blocked.node_id()).await.unwrap(),
            LookupOutcome::Pending
        );
    }

    #[tokio::test]
    async fn 信令协议版本不匹配会被拒绝() {
        let addr = spawn_signal_server().await;
        let alice = Identity::generate();
        let mut raw = Raw::connect(addr).await;
        raw.send(&SignalMessage::RegisterHello {
            protocol_version: RECOVERY_PROTOCOL_VERSION + 1,
            node_id: alice.node_id(),
            public_key: alice.public_key_bytes(),
        })
        .await;

        match raw.recv().await {
            Some(SignalMessage::Error { reason }) => {
                assert!(reason.contains("版本"), "实际: {reason}");
            }
            other => panic!("版本不匹配必须明确拒绝，实际 {other:?}"),
        }
    }

    #[tokio::test]
    async fn 客户端拒绝版本不匹配的挑战() {
        // 假服务器回一个版本号不对的 challenge，客户端必须明确报错，
        // 不能当作正常流程继续。
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let (read_half, mut write_half) = stream.into_split();
            let mut reader = BufReader::new(read_half);
            let _ = read_raw_frame_limited(&mut reader, MAX_SIGNAL_FRAME_LEN).await;
            let reply = SignalMessage::RegisterChallenge {
                protocol_version: SIGNAL_PROTOCOL_VERSION + 1,
                challenge: [0u8; CHALLENGE_LEN],
            };
            let _ = write_raw_frame_limited(
                &mut write_half,
                &reply.encode().unwrap(),
                MAX_SIGNAL_FRAME_LEN,
            )
            .await;
        });

        let alice = Identity::generate();
        let err = SignalingClient::connect(&addr.to_string(), &alice, vec![])
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Discovery(_)), "实际 {err:?}");
        assert!(err.to_string().contains("版本"), "实际: {err}");
    }

    #[test]
    fn 线格式变体序号保持稳定() {
        // postcard 的 enum 用变体序号编码，而这些序号就是 v1 的线格式：一旦
        // 重排，跨版本会**静默错解**（例如 v1 客户端把 v2 的 RegisterChallenge
        // 当成 Registered，假装登记成功了）。这里把序号钉死，防止以后不小心改。
        let node = Identity::generate().node_id();
        let index = |message: &SignalMessage| message.encode().unwrap()[0];

        assert_eq!(
            index(&SignalMessage::Register {
                node_id: node,
                public_key: [0u8; 32],
                candidates: vec![],
                signature: vec![]
            }),
            0
        );
        assert_eq!(index(&SignalMessage::Registered), 1);
        assert_eq!(index(&SignalMessage::Lookup { node_id: node }), 2);
        assert_eq!(
            index(&SignalMessage::PeerCandidates {
                node_id: node,
                candidates: vec![],
                token: PunchToken::from_bytes([0u8; 16]),
            }),
            3
        );
        assert_eq!(index(&SignalMessage::PeerPending { node_id: node }), 4);
        assert_eq!(index(&SignalMessage::Ping), 5);
        assert_eq!(index(&SignalMessage::Pong), 6);
        assert_eq!(
            index(&SignalMessage::Error {
                reason: String::new()
            }),
            7
        );
        assert_eq!(
            index(&SignalMessage::RegisterHello {
                protocol_version: SIGNAL_PROTOCOL_VERSION,
                node_id: node,
                public_key: [0u8; 32],
            }),
            8
        );
        assert_eq!(
            index(&SignalMessage::RegisterChallenge {
                protocol_version: SIGNAL_PROTOCOL_VERSION,
                challenge: [0u8; CHALLENGE_LEN],
            }),
            9
        );
    }

    #[tokio::test]
    async fn v1_无签名注册帧会被明确拒绝() {
        // 手工拼一个 v1 的 `Register` 帧：变体 0 + node_id + public_key +
        // candidates，没有 signature 字段。服务器必须解析失败并拒绝，
        // 绝不能把它当成一次合法登记（那正是 #2 要修的冒名路径）。
        let addr = spawn_signal_server().await;
        let alice = Identity::generate();

        let mut body = vec![0u8];
        body.extend(postcard::to_allocvec(&alice.node_id()).unwrap());
        body.extend(postcard::to_allocvec(&alice.public_key_bytes()).unwrap());
        body.extend(
            postcard::to_allocvec(&vec![candidate(CandidateKind::Host, "192.168.1.9:9000")])
                .unwrap(),
        );

        let mut raw = Raw::connect(addr).await;
        write_raw_frame_limited(&mut raw.stream, &body, MAX_SIGNAL_FRAME_LEN)
            .await
            .unwrap();

        let reply = tokio::time::timeout(Duration::from_secs(3), raw.recv())
            .await
            .expect("服务端不该挂死");
        assert!(
            !matches!(reply, Some(SignalMessage::Registered)),
            "v1 无签名注册帧必须被拒绝，实际 {reply:?}"
        );
    }
}
