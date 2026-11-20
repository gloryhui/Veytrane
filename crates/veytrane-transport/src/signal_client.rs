//! Signaling client registration, heartbeat, lookup, and event handling.

use crate::error::{Error, Result};
use crate::identity::{Identity, NodeId};
use crate::punch::PunchToken;
use crate::raw_frame::{read_raw_frame_limited, write_raw_frame_limited};
use crate::short_id::ShortId;
use crate::signal_tls::SignalTlsClient;
use crate::signal_wire::{
    CHALLENGE_LEN, Candidate, MAX_CANDIDATES, MAX_SIGNAL_FRAME_LEN, OUTBOX_CAPACITY,
    RECOVERY_PROTOCOL_VERSION, SHORT_ID_PROTOCOL_VERSION, SHORT_LOOKUP_UNAVAILABLE,
    SIGNAL_PROTOCOL_VERSION, SignalMessage, recovery_payload, register_payload,
};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, BufReader, ReadHalf, WriteHalf};
use tokio::net::TcpStream;
use tokio::sync::mpsc;

#[doc(hidden)]
pub trait SignalIo: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> SignalIo for T {}
#[doc(hidden)]
pub type SignalStream = Box<dyn SignalIo>;
type SignalRead = ReadHalf<SignalStream>;
#[doc(hidden)]
pub type SignalWrite = WriteHalf<SignalStream>;

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

    #[doc(hidden)]
    pub async fn connect_with_options(
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

    #[doc(hidden)]
    pub async fn request_recovery_lookup(
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
