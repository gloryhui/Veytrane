//! QUIC 打洞 UDP 复用与诊断。
//!
//! 通用 TLS/ALPN、端点构建和基础连接链位于 `veytrane_transport::quic`，
//! 这里保留旧公开路径并只承载打洞专用的接收分发。
//!
//! # 关于跳过 TLS 证书校验
//!
//! P2P 场景里没有 CA，也不该有：节点身份由 Ed25519 公钥决定，公钥到节点 ID
//! 的映射是自证的。所以传输层**故意**使用不校验证书的 verifier，信任建立
//! 推迟到应用层握手（见 [`crate::transport::handshake`]）。
//!
//! 成立的前提是：**任何连接都必须先跑完应用层握手**。QUIC 提供的是加密和
//! 完整性，握手提供的是身份确认；少了握手，中间人就挡不住了。
//! 这条约束不能省。
//!
//! 但「跑了握手」还不够：如果签名载荷不含任何和当前 TLS 会话相关的东西，
//! 攻击者可以建立两条独立连接（A↔M、M↔B）原样转发握手消息，让 A、B 各自
//! 都验证通过。为此应用层握手必须把 [`ChannelBinding`]（由当前连接的 TLS
//! exporter 导出）纳入签名载荷，见 [`crate::transport::handshake`]。

use std::collections::{HashMap, HashSet};
use std::io::{self, IoSliceMut};
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, ready};
use std::time::Duration;

use quinn::{AsyncUdpSocket, Endpoint, UdpPoller};
use tokio::io::Interest;
use tokio::net::UdpSocket;
use tokio::sync::mpsc;

use crate::error::{Error, Result};
use crate::nat::punch::{PROBE_MAGIC, PunchToken};

const PUNCH_PROBE_PADDING_LEN: usize = 16;
const PUNCH_EVENT_CAPACITY: usize = 4;
const MAX_PUNCH_SOURCES_PER_ROUTE: usize = 8;
const MAX_AUTHORIZED_PUNCH_SOURCES: usize = 128;

/// 等待 QUIC transport handshake 完成的最大时间；与应用层四阶段握手分开计时。
pub const QUIC_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// 等待一条连接的第一条双向业务流的最大时间。
pub const ACCEPT_FIRST_BI_STREAM_TIMEOUT: Duration = Duration::from_secs(10);

/// 等待业务流首帧的最大时间。
pub const STREAM_FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(10);

pub use veytrane_transport::TRANSFER_IDLE_TIMEOUT;
pub use veytrane_transport::channel_binding::{
    CHANNEL_BINDING_LABEL, CHANNEL_BINDING_LEN, ChannelBinding,
};
pub use veytrane_transport::handshake::APPLICATION_HANDSHAKE_TIMEOUT;
pub use veytrane_transport::quic::{
    ALPN, KEEP_ALIVE_INTERVAL, MAX_IDLE_TIMEOUT, client_endpoint, connect, endpoint_from_socket,
    install_crypto_provider, server_endpoint,
};

pub(crate) fn endpoint_from_socket_with_config(
    socket: std::net::UdpSocket,
    endpoint_config: quinn::EndpointConfig,
) -> Result<Endpoint> {
    veytrane_transport::quic::endpoint_from_socket_with_config(socket, endpoint_config)
}

/// A send-only view of the UDP socket whose receive side is owned by Quinn.
///
/// Desktop punch traffic is demultiplexed inside Quinn's `AsyncUdpSocket::poll_recv`
/// implementation. Callers can register a token and send probes, but cannot create a
/// competing receive loop on this handle.
#[derive(Clone, Debug)]
pub struct PunchSocketHandle {
    io: Arc<UdpSocket>,
    router: PunchRouter,
}

impl PunchSocketHandle {
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.io.local_addr()
    }

    /// Refresh the STUN mapping on the exact socket later used for QUIC.
    /// An Indication expects no response and cannot compete with Quinn's reads.
    pub async fn keep_mapping_alive(&self, server: SocketAddr) -> io::Result<()> {
        if !crate::net::AddressFamily::of(self.local_addr()?).accepts(server) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "STUN keepalive family mismatch",
            ));
        }
        let packet = crate::nat::stun::Message {
            method: crate::nat::stun::Method::Binding,
            class: crate::nat::stun::MessageClass::Indication,
            transaction_id: rand::random(),
            attributes: Vec::new(),
        }
        .encode();
        self.io.send_to(&packet, server).await?;
        Ok(())
    }

    pub fn register_probe(&self, token: &PunchToken) -> Result<PunchProbeReceiver> {
        let local_addr = self.router.local_addr;
        let result = self.router.register(token, None, 0);
        #[cfg(feature = "node-runtime")]
        crate::transport::diagnostic::punch_watch_register(
            local_addr,
            token,
            result.as_ref().ok().map(|receiver| receiver.id),
            None,
            0,
            result.is_ok(),
        );
        result
    }

    pub fn register_peer_probe(
        &self,
        token: &PunchToken,
        peer: crate::identity::NodeId,
        generation: u64,
    ) -> Result<PunchProbeReceiver> {
        let local_addr = self.router.local_addr;
        let result = self.router.register(token, Some(peer), generation);
        #[cfg(feature = "node-runtime")]
        crate::transport::diagnostic::punch_watch_register(
            local_addr,
            token,
            result.as_ref().ok().map(|receiver| receiver.id),
            Some(peer),
            generation,
            result.is_ok(),
        );
        result
    }

    /// Consume an address authorization created by a token-validated punch datagram.
    pub fn claim_authorized_peer(
        &self,
        source: SocketAddr,
    ) -> Option<(crate::identity::NodeId, u64)> {
        self.router.claim_authorized_peer(source)
    }

    pub async fn send_probe_to(
        &self,
        token: &PunchToken,
        destination: SocketAddr,
    ) -> io::Result<()> {
        let local = self.local_addr()?;
        if !crate::net::AddressFamily::of(local).accepts(destination) {
            let error = io::Error::new(
                io::ErrorKind::InvalidInput,
                "punch address family mismatch",
            );
            #[cfg(feature = "node-runtime")]
            crate::transport::diagnostic::punch_send_error(
                Some(local),
                token,
                destination,
                &error,
            );
            return Err(error);
        }
        let packet = crate::nat::punch::probe_packet(token);
        let sent = match self.io.send_to(&packet, destination).await {
            Ok(sent) => sent,
            Err(error) => {
                #[cfg(feature = "node-runtime")]
                crate::transport::diagnostic::punch_send_error(
                    Some(local),
                    token,
                    destination,
                    &error,
                );
                return Err(error);
            }
        };
        if sent != packet.len() {
            let error = io::Error::new(
                io::ErrorKind::WriteZero,
                "UDP punch probe was only partially sent",
            );
            #[cfg(feature = "node-runtime")]
            crate::transport::diagnostic::punch_send_error(
                Some(local),
                token,
                destination,
                &error,
            );
            return Err(error);
        }
        #[cfg(feature = "node-runtime")]
        crate::transport::diagnostic::punch_send_ok(Some(local), token, destination);
        Ok(())
    }
}

#[derive(Debug)]
struct PunchRoute {
    id: u64,
    expected_peer: Option<crate::identity::NodeId>,
    generation: u64,
    sender: mpsc::Sender<SocketAddr>,
    sources: HashSet<SocketAddr>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct AuthorizedPunchSource {
    peer: crate::identity::NodeId,
    generation: u64,
    route_id: u64,
}

#[derive(Debug, Default)]
struct PunchRoutes {
    by_token: HashMap<[u8; 16], PunchRoute>,
    by_source: HashMap<SocketAddr, Vec<AuthorizedPunchSource>>,
    source_count: usize,
}

#[derive(Clone, Debug, Default)]
struct PunchRouter {
    routes: Arc<Mutex<PunchRoutes>>,
    next_id: Arc<AtomicU64>,
    local_addr: Option<SocketAddr>,
}

const MAX_PUNCH_ROUTES: usize = 48;

impl PunchRouter {
    fn register(
        &self,
        token: &PunchToken,
        expected_peer: Option<crate::identity::NodeId>,
        generation: u64,
    ) -> Result<PunchProbeReceiver> {
        let (sender, receiver) = mpsc::channel(PUNCH_EVENT_CAPACITY);
        let id = self.next_id.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
        let key = *token.as_bytes();
        let mut routes = self
            .routes
            .lock()
            .map_err(|_| Error::Transport("打洞事件路由锁已损坏".into()))?;
        // Desktop keeps at most two recent reconnect tokens per connected peer,
        // plus an active handshake route, with at most 16 peers.
        if routes.by_token.len() >= MAX_PUNCH_ROUTES {
            return Err(Error::Transport("打洞令牌注册已达资源上限".into()));
        }
        if routes.by_token.contains_key(&key) {
            return Err(Error::Transport("同一打洞令牌已有活动接收者".into()));
        }
        routes.by_token.insert(
            key,
            PunchRoute {
                id,
                expected_peer,
                generation,
                sender,
                sources: HashSet::new(),
            },
        );
        Ok(PunchProbeReceiver {
            token: key,
            id,
            router: self.clone(),
            receiver,
            local_addr: self.local_addr,
            expected_peer,
            generation,
        })
    }

    /// Return true only for an exact-size probe carrying a currently registered token.
    /// This keeps ordinary QUIC packets on Quinn's path, even if their bytes happen to
    /// share the probe magic prefix.
    fn route_datagram(&self, packet: &[u8], source: SocketAddr) -> bool {
        let token_start = PROBE_MAGIC.len();
        let expected_len = token_start + 16 + PUNCH_PROBE_PADDING_LEN;
        if packet.len() != expected_len || !packet.starts_with(PROBE_MAGIC) {
            return false;
        }
        let mut token = [0u8; 16];
        let token_end = token_start + token.len();
        token.copy_from_slice(&packet[token_start..token_end]);
        let mut routes = match self.routes.lock() {
            Ok(routes) => routes,
            // The packet has the exact punch shape, so drop it if internal routing is
            // unavailable. Passing it to QUIC must never create punch progress.
            Err(_) => {
                #[cfg(feature = "node-runtime")]
                crate::transport::diagnostic::punch_recv(
                    self.local_addr,
                    &PunchToken::from_bytes(token),
                    source,
                    "unknown",
                    None,
                    None,
                    Some("routes_unavailable"),
                );
                return true;
            }
        };
        let Some(route) = routes.by_token.get(&token) else {
            drop(routes);
            #[cfg(feature = "node-runtime")]
            crate::transport::diagnostic::punch_recv(
                self.local_addr,
                &PunchToken::from_bytes(token),
                source,
                "unknown",
                None,
                None,
                None,
            );
            return false;
        };
        let route_id = route.id;
        let expected_peer = route.expected_peer;
        let generation = route.generation;
        #[cfg(feature = "node-runtime")]
        crate::transport::diagnostic::punch_recv(
            self.local_addr,
            &PunchToken::from_bytes(token),
            source,
            "matched",
            expected_peer,
            Some(generation),
            expected_peer.is_none().then_some("unbound_watch"),
        );
        let already_registered = route.sources.contains(&source);
        let sender = route.sender.clone();
        if let Some(peer) = expected_peer
            && !already_registered
        {
            if route.sources.len() >= MAX_PUNCH_SOURCES_PER_ROUTE
                || routes.source_count >= MAX_AUTHORIZED_PUNCH_SOURCES
            {
                #[cfg(feature = "node-runtime")]
                crate::transport::diagnostic::punch_recv(
                    self.local_addr,
                    &PunchToken::from_bytes(token),
                    source,
                    "matched",
                    Some(peer),
                    Some(generation),
                    Some("source_limit"),
                );
                return true;
            }
            routes
                .by_token
                .get_mut(&token)
                .expect("route was checked above")
                .sources
                .insert(source);
            routes
                .by_source
                .entry(source)
                .or_default()
                .push(AuthorizedPunchSource {
                    peer,
                    generation,
                    route_id,
                });
            routes.source_count += 1;
        }
        drop(routes);
        let _ = sender.try_send(source);
        true
    }

    fn claim_authorized_peer(&self, source: SocketAddr) -> Option<(crate::identity::NodeId, u64)> {
        let mut routes = self.routes.lock().ok()?;
        let authorizations = routes.by_source.get(&source)?;
        let mut active_authorizations = authorizations.iter().filter(|authorization| {
            routes
                .by_token
                .values()
                .any(|route| route.id == authorization.route_id)
        });
        let authorization = *active_authorizations.next()?;
        // More than one live token advertised this source. Keep the entries so that
        // unregistering a stale route can make a later incoming QUIC connection
        // unambiguous instead of permanently consuming the authorization here.
        if active_authorizations.next().is_some() {
            #[cfg(feature = "node-runtime")]
            crate::transport::diagnostic::punch_claim(
                self.local_addr,
                source,
                "ambiguous",
                Some(authorization.peer),
                Some(authorization.generation),
                authorizations
                    .iter()
                    .filter(|authorization| {
                        routes
                            .by_token
                            .values()
                            .any(|route| route.id == authorization.route_id)
                    })
                    .count(),
            );
            return None;
        }

        if let Some(authorizations) = routes.by_source.remove(&source) {
            routes.source_count = routes.source_count.saturating_sub(authorizations.len());
        }
        for route in routes.by_token.values_mut() {
            route.sources.remove(&source);
        }
        #[cfg(feature = "node-runtime")]
        crate::transport::diagnostic::punch_claim(
            self.local_addr,
            source,
            "matched",
            Some(authorization.peer),
            Some(authorization.generation),
            1,
        );
        Some((authorization.peer, authorization.generation))
    }

    fn unregister(&self, token: &[u8; 16], id: u64) {
        if let Ok(mut routes) = self.routes.lock()
            && routes
                .by_token
                .get(token)
                .is_some_and(|route| route.id == id)
            && let Some(route) = routes.by_token.remove(token)
        {
            for source in route.sources {
                if let Some(mut authorizations) = routes.by_source.remove(&source) {
                    let old_len = authorizations.len();
                    authorizations.retain(|authorization| authorization.route_id != id);
                    let removed = old_len - authorizations.len();
                    routes.source_count = routes.source_count.saturating_sub(removed);
                    if authorizations.is_empty() {
                        continue;
                    }
                    routes.by_source.insert(source, authorizations);
                }
            }
        }
    }
}

/// Receives validated sources for one punch token. Dropping it unregisters that token.
#[derive(Debug)]
pub struct PunchProbeReceiver {
    token: [u8; 16],
    id: u64,
    router: PunchRouter,
    receiver: mpsc::Receiver<SocketAddr>,
    local_addr: Option<SocketAddr>,
    expected_peer: Option<crate::identity::NodeId>,
    generation: u64,
}

impl PunchProbeReceiver {
    pub fn poll_recv(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<SocketAddr>> {
        self.receiver.poll_recv(cx)
    }
    pub async fn recv(&mut self) -> Option<SocketAddr> {
        self.receiver.recv().await
    }
}

impl Drop for PunchProbeReceiver {
    fn drop(&mut self) {
        self.router.unregister(&self.token, self.id);
        #[cfg(feature = "node-runtime")]
        crate::transport::diagnostic::punch_watch_drop(
            self.local_addr,
            &PunchToken::from_bytes(self.token),
            self.id,
            self.expected_peer,
            self.generation,
        );
    }
}

/// Build a bidirectional QUIC endpoint and a probe sender over one bound UDP socket.
///
/// Quinn remains the only receive owner. Its adapter consumes only exact-size punch
/// probes whose random token has an active registration; every other datagram is passed
/// to Quinn unchanged.
pub fn endpoint_from_socket_with_punch_dispatcher(
    socket: std::net::UdpSocket,
) -> Result<(Endpoint, PunchSocketHandle)> {
    install_crypto_provider();
    socket
        .set_nonblocking(true)
        .map_err(|err| Error::Transport(format!("设置非阻塞失败: {err}")))?;
    let io = Arc::new(
        UdpSocket::from_std(socket)
            .map_err(|err| Error::Transport(format!("接管 UDP socket 失败: {err}")))?,
    );
    let router = PunchRouter {
        local_addr: io.local_addr().ok(),
        ..PunchRouter::default()
    };
    let state = quinn::udp::UdpSocketState::new((&*io).into())
        .map_err(|err| Error::Transport(format!("初始化 QUIC UDP 状态失败: {err}")))?;
    let adapter: Arc<dyn AsyncUdpSocket> = Arc::new(PunchAwareUdpSocket {
        io: Arc::clone(&io),
        state,
        router: router.clone(),
    });
    let runtime = quinn::default_runtime()
        .ok_or_else(|| Error::Transport("当前没有可用的异步运行时".into()))?;
    let endpoint = veytrane_transport::quic::endpoint_from_abstract_socket(adapter, runtime)?;
    Ok((endpoint, PunchSocketHandle { io, router }))
}

#[derive(Debug)]
struct PunchAwareUdpSocket {
    io: Arc<UdpSocket>,
    state: quinn::udp::UdpSocketState,
    router: PunchRouter,
}

impl PunchAwareUdpSocket {
    fn filter_probes(
        &self,
        buffers: &mut [IoSliceMut<'_>],
        metadata: &mut [quinn::udp::RecvMeta],
        received: usize,
    ) -> io::Result<usize> {
        let mut output = 0;
        for input in 0..received {
            let original = metadata[input];
            if original.len == 0 {
                continue; // A legal empty UDP datagram is not an endpoint I/O error.
            }
            if original.stride == 0 || original.len > buffers[input].len() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "QUIC UDP receive metadata is invalid",
                ));
            }

            let mut read_offset = 0;
            let mut write_offset = 0;
            let mut kept = 0usize;
            let mut final_len = 0usize;
            while read_offset < original.len {
                let packet_len = (original.len - read_offset).min(original.stride);
                let packet_end = read_offset + packet_len;
                let source = original.addr;
                let routed = self
                    .router
                    .route_datagram(&buffers[input][read_offset..packet_end], source);
                if !routed {
                    if read_offset != write_offset {
                        buffers[input].copy_within(read_offset..packet_end, write_offset);
                    }
                    write_offset += packet_len;
                    kept += 1;
                    final_len = packet_len;
                }
                read_offset += original.stride;
            }

            if kept == 0 {
                continue;
            }
            if input != output {
                let (before, after) = buffers.split_at_mut(input);
                before[output][..write_offset].copy_from_slice(&after[0][..write_offset]);
            }
            let mut kept_metadata = original;
            kept_metadata.len = write_offset;
            if kept == 1 {
                kept_metadata.stride = final_len;
            }
            metadata[output] = kept_metadata;
            output += 1;
        }
        Ok(output)
    }
}

impl AsyncUdpSocket for PunchAwareUdpSocket {
    fn create_io_poller(self: Arc<Self>) -> Pin<Box<dyn UdpPoller>> {
        Box::pin(PunchUdpPoller {
            io: Arc::clone(&self.io),
        })
    }

    fn try_send(&self, transmit: &quinn::udp::Transmit<'_>) -> io::Result<()> {
        self.io.try_io(Interest::WRITABLE, || {
            self.state.try_send((&*self.io).into(), transmit)
        })
    }

    fn poll_recv(
        &self,
        cx: &mut Context<'_>,
        buffers: &mut [IoSliceMut<'_>],
        metadata: &mut [quinn::udp::RecvMeta],
    ) -> Poll<io::Result<usize>> {
        for _ in 0..32 {
            ready!(self.io.poll_recv_ready(cx))?;
            let received = match self.io.try_io(Interest::READABLE, || {
                self.state.recv((&*self.io).into(), buffers, metadata)
            }) {
                Ok(received) => received,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => continue,
                Err(error) => return Poll::Ready(Err(error)),
            };
            match self.filter_probes(buffers, metadata, received) {
                Ok(0) => continue,
                Ok(remaining) => return Poll::Ready(Ok(remaining)),
                Err(error) => return Poll::Ready(Err(error)),
            }
        }
        cx.waker().wake_by_ref();
        Poll::Pending
    }

    fn local_addr(&self) -> io::Result<SocketAddr> {
        self.io.local_addr()
    }

    fn max_transmit_segments(&self) -> usize {
        self.state.max_gso_segments()
    }

    fn max_receive_segments(&self) -> usize {
        self.state.gro_segments()
    }

    fn may_fragment(&self) -> bool {
        self.state.may_fragment()
    }
}

#[derive(Debug)]
struct PunchUdpPoller {
    io: Arc<UdpSocket>,
}

impl UdpPoller for PunchUdpPoller {
    fn poll_writable(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.io.poll_send_ready(cx)
    }
}

/// Dedicated identity streams are already finished by existing CLI/Desktop
/// initiators after Ready. Only the racing winner sends that FIN; responders
/// must observe it before exposing capabilities, authorization or handlers.
pub async fn await_identity_commit(recv: &mut quinn::RecvStream) -> Result<()> {
    let mut byte = [0_u8; 1];
    let next = tokio::time::timeout(APPLICATION_HANDSHAKE_TIMEOUT, recv.read(&mut byte))
        .await
        .map_err(|_| Error::Transport("identity winner commit timed out".into()))?
        .map_err(|e| Error::Transport(e.to_string()))?;
    if next.is_some() {
        return Err(Error::Protocol(
            "unexpected data on dedicated identity stream".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn large_stream_survives_loss_without_starving_reverse_traffic() {
        // A deterministic last-mile path: 20ms RTT, one in six downstream
        // datagrams lost after handshake. Exercise the actual punch adapter.
        let (server, _) = endpoint_from_socket_with_punch_dispatcher(
            crate::net::family::bind_udp("127.0.0.1:0".parse().unwrap()).unwrap(),
        )
        .unwrap();
        let server_addr = server.local_addr().unwrap();
        let proxy = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let proxy_addr = proxy.local_addr().unwrap();
        let router = tokio::spawn(async move {
            let mut downstream = 0u64;
            let mut client = None;
            let mut buffer = vec![0; 65536];
            let mut deliveries = tokio::task::JoinSet::new();
            loop {
                let (size, from) = proxy.recv_from(&mut buffer).await.unwrap();
                let to = if from == server_addr {
                    downstream += 1;
                    if downstream > 32 && downstream.is_multiple_of(6) {
                        continue;
                    }
                    client.unwrap()
                } else {
                    client = Some(from);
                    server_addr
                };
                let bytes = buffer[..size].to_vec();
                let socket = proxy.clone();
                deliveries.spawn(async move {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    socket.send_to(&bytes, to).await.unwrap();
                });
                while deliveries.try_join_next().is_some() {}
            }
        });
        let (client, _) = endpoint_from_socket_with_punch_dispatcher(
            crate::net::family::bind_udp("127.0.0.1:0".parse().unwrap()).unwrap(),
        )
        .unwrap();
        let accept = tokio::spawn(async move {
            let connection = server.accept().await.unwrap().await.unwrap();
            (server, connection)
        });
        let connection = connect(&client, proxy_addr, "127.0.0.1").await.unwrap();
        let (server, remote) = accept.await.unwrap();
        let bytes = vec![0x5a; 2 * 1024 * 1024];
        let expected = blake3::hash(&bytes);
        let sender = tokio::spawn(async move {
            let (mut send, mut recv) = remote.accept_bi().await.unwrap();
            let reverse = tokio::spawn(async move {
                let mut received = 0;
                let mut buffer = [0; 32];
                while let Some(n) = recv.read(&mut buffer).await.unwrap() {
                    received += n;
                }
                received
            });
            send.write_all(&bytes).await.unwrap();
            send.finish().unwrap();
            send.stopped().await.unwrap();
            reverse.await.unwrap()
        });
        let (mut send, mut recv) = connection.open_bi().await.unwrap();
        let heartbeat = tokio::spawn(async move {
            for _ in 0..20 {
                send.write_all(b"ping").await.unwrap();
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
            send.finish().unwrap();
        });
        let started = std::time::Instant::now();
        let result =
            tokio::time::timeout(Duration::from_secs(8), recv.read_to_end(3 * 1024 * 1024)).await;
        heartbeat.await.unwrap();
        // Close before assertions, so a regression cannot leave the proxy running.
        let data = result.as_ref().ok().and_then(|r| r.as_ref().ok());
        let intact = data.is_some_and(|b| blake3::hash(b) == expected);
        let reverse_bytes = if intact {
            sender.await.unwrap()
        } else {
            sender.abort();
            0
        };
        let stats = connection.stats();
        connection.close(0u32.into(), b"done");
        client.wait_idle().await;
        server.close(0u32.into(), b"done");
        server.wait_idle().await;
        router.abort();
        eprintln!(
            "2MiB lossy stream: {:?}, intact={intact}, reverse={reverse_bytes}, received={}",
            started.elapsed(),
            stats.udp_rx.bytes
        );
        assert!(intact, "large stream stalled on a lossy path: {result:?}");
        assert_eq!(reverse_bytes, 80);
    }

    #[tokio::test]
    async fn idle_mapping_keepalive_uses_the_quic_port_and_preserves_punch_routing() {
        use crate::nat::stun::{Message, MessageClass, Method};
        let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let (endpoint, punch) = endpoint_from_socket_with_punch_dispatcher(socket).unwrap();
        let stun = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        punch
            .keep_mapping_alive(stun.local_addr().unwrap())
            .await
            .unwrap();
        let mut bytes = [0; 1500];
        let (size, source) =
            tokio::time::timeout(Duration::from_secs(2), stun.recv_from(&mut bytes))
                .await
                .unwrap()
                .unwrap();
        assert_eq!(source, endpoint.local_addr().unwrap());
        let packet = Message::decode(&bytes[..size]).unwrap();
        assert_eq!(packet.method, Method::Binding);
        assert_eq!(packet.class, MessageClass::Indication);
        assert!(packet.attributes.is_empty());
        let token = PunchToken::random();
        let mut route = punch.register_probe(&token).unwrap();
        stun.send_to(&crate::nat::punch::probe_packet(&token), source)
            .await
            .unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), route.recv())
                .await
                .unwrap(),
            Some(stun.local_addr().unwrap())
        );
        endpoint.close(0u32.into(), b"test complete");
    }
    use super::*;
    use crate::identity::Identity;
    use crate::nat::punch::{PunchToken, probe_packet};

    #[test]
    fn 加密后端可以重复安装() {
        install_crypto_provider();
        install_crypto_provider();
    }

    #[tokio::test]
    async fn punch_filter_preserves_quic_segments_and_ignores_empty_udp() {
        let io = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let adapter = PunchAwareUdpSocket {
            state: quinn::udp::UdpSocketState::new((&*io).into()).unwrap(),
            io,
            router: PunchRouter::default(),
        };
        let token = PunchToken::random();
        let mut route = adapter.router.register(&token, None, 0).unwrap();
        let probe = probe_packet(&token);
        let quic = vec![0xc0; probe.len()];
        let mut batch = [probe.as_slice(), quic.as_slice(), probe.as_slice()].concat();
        let mut empty = [0; 256];
        let mut output = [0; 256];
        let source = "127.0.0.1:32123".parse().unwrap();
        let mut metadata = [quinn::udp::RecvMeta::default(); 3];
        metadata[0].addr = source;
        metadata[1].addr = source;
        metadata[1].len = batch.len();
        metadata[1].stride = probe.len();
        output[..3].copy_from_slice(b"xyz");
        metadata[2].addr = source;
        metadata[2].len = 3;
        metadata[2].stride = 3;
        let mut buffers = [
            IoSliceMut::new(&mut empty),
            IoSliceMut::new(&mut batch),
            IoSliceMut::new(&mut output),
        ];
        assert_eq!(
            adapter
                .filter_probes(&mut buffers, &mut metadata, 3)
                .unwrap(),
            2
        );
        assert_eq!(&buffers[0][..metadata[0].len], quic);
        assert_eq!(&buffers[1][..metadata[1].len], b"xyz");
        assert_eq!(route.recv().await, Some(source));
    }

    #[test]
    fn punch_route_capacity_is_bounded_and_reclaimed_on_drop() {
        let router = PunchRouter::default();
        let peer = Identity::generate().node_id();
        let mut routes: Vec<_> = (0..MAX_PUNCH_ROUTES)
            .map(|generation| {
                router
                    .register(&PunchToken::random(), Some(peer), generation as u64)
                    .unwrap()
            })
            .collect();
        let token = PunchToken::random();
        assert!(router.register(&token, Some(peer), 99).is_err());
        routes.pop();
        let receiver = router.register(&token, Some(peer), 99).unwrap();
        assert_eq!(
            router.routes.lock().unwrap().by_token.len(),
            MAX_PUNCH_ROUTES
        );
        drop((routes, receiver));
        assert!(router.routes.lock().unwrap().by_token.is_empty());
    }

    #[test]
    fn ambiguous_punch_source_cannot_authorize_wrong_peer_or_outlive_route() {
        let router = PunchRouter::default();
        let a = Identity::generate().node_id();
        let b = Identity::generate().node_id();
        let token_a = PunchToken::random();
        let token_b = PunchToken::random();
        let route_a = router.register(&token_a, Some(a), 1).unwrap();
        let route_b = router.register(&token_b, Some(b), 2).unwrap();
        let source = "127.0.0.1:32000".parse().unwrap();
        assert!(router.route_datagram(&probe_packet(&token_a), source));
        assert!(router.route_datagram(&probe_packet(&token_b), source));
        assert_eq!(router.claim_authorized_peer(source), None);
        drop(route_a);
        assert_eq!(router.claim_authorized_peer(source), Some((b, 2)));
        drop(route_b);
        assert!(!router.route_datagram(&probe_packet(&token_b), source));
        assert_eq!(router.claim_authorized_peer(source), None);
    }

    #[tokio::test]
    async fn quinn接收所有者只分发匹配令牌的打洞包() {
        let socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let expected = socket.local_addr().unwrap();
        let (endpoint, punch) = endpoint_from_socket_with_punch_dispatcher(socket).unwrap();
        assert_eq!(endpoint.local_addr().unwrap(), expected);

        let token = PunchToken::from_bytes([0x24; 16]);
        let wrong_token = PunchToken::from_bytes([0x42; 16]);
        let expected_peer = Identity::generate().node_id();
        let generation = 11;
        let mut receiver = punch
            .register_peer_probe(&token, expected_peer, generation)
            .unwrap();
        let sender = UdpSocket::bind("127.0.0.1:0").await.unwrap();

        sender
            .send_to(&probe_packet(&wrong_token), expected)
            .await
            .unwrap();
        sender
            .send_to(b"P2PF-PUNCH/1 truncated", expected)
            .await
            .unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(100), receiver.recv())
                .await
                .is_err()
        );
        assert_eq!(
            punch.claim_authorized_peer(sender.local_addr().unwrap()),
            None
        );

        sender
            .send_to(&probe_packet(&token), expected)
            .await
            .unwrap();
        let source = tokio::time::timeout(Duration::from_secs(2), receiver.recv())
            .await
            .expect("匹配令牌的包应由 Quinn 的唯一接收所有者分发")
            .expect("令牌接收者仍应处于注册状态");
        assert_eq!(source, sender.local_addr().unwrap());
        assert_eq!(
            punch.claim_authorized_peer(source),
            Some((expected_peer, generation))
        );

        drop(receiver);
        endpoint.close(0u32.into(), b"test complete");
        endpoint.wait_idle().await;
    }

    #[cfg(feature = "standalone")]
    #[tokio::test]
    async fn 用现成_socket_建的端点能连上() {
        use crate::protocol::frame::{read_frame, write_frame};
        use crate::protocol::message::ControlMessage;

        let server_socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let server_addr = server_socket.local_addr().unwrap();
        let server = endpoint_from_socket(server_socket).unwrap();

        let client_socket = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let client = endpoint_from_socket(client_socket).unwrap();

        let server_task = tokio::spawn(async move {
            let incoming = server.accept().await.expect("应当收到连接");
            let connection = incoming.await.expect("握手应当成功");

            let (mut send, mut recv) = connection.accept_bi().await.expect("应当收到流");
            let request = read_frame(&mut recv).await.unwrap().unwrap();
            assert_eq!(request, ControlMessage::KeepAlive);

            write_frame(&mut send, &ControlMessage::Ready)
                .await
                .unwrap();
            send.finish().unwrap();

            connection.closed().await;
            server.wait_idle().await;
        });

        let connection = connect(&client, server_addr, "127.0.0.1").await.unwrap();
        let (mut send, mut recv) = connection.open_bi().await.unwrap();
        write_frame(&mut send, &ControlMessage::KeepAlive)
            .await
            .unwrap();

        assert_eq!(
            read_frame(&mut recv).await.unwrap().unwrap(),
            ControlMessage::Ready
        );

        connection.close(0u32.into(), b"bye");
        client.wait_idle().await;
        server_task.await.unwrap();
    }

    #[cfg(feature = "standalone")]
    #[tokio::test]
    async fn 端点能在环回地址上收发数据() {
        use crate::protocol::frame::{read_frame, write_frame};
        use crate::protocol::message::ControlMessage;

        let server = server_endpoint("127.0.0.1:0".parse().unwrap()).unwrap();
        let server_addr = server.local_addr().unwrap();

        let server_task = tokio::spawn(async move {
            let incoming = server.accept().await.expect("应当收到连接");
            let connection = incoming.await.expect("握手应当成功");

            let (mut send, mut recv) = connection.accept_bi().await.expect("应当收到流");
            let request = read_frame(&mut recv).await.unwrap().unwrap();
            assert_eq!(request, ControlMessage::RequestChunk { index: 5 });

            write_frame(
                &mut send,
                &ControlMessage::Chunk {
                    index: 5,
                    data: b"hello from server".to_vec(),
                },
            )
            .await
            .unwrap();
            send.finish().unwrap();

            // 等客户端确认收到，避免连接被提前关掉。
            connection.closed().await;
            server.wait_idle().await;
        });

        let client = client_endpoint("127.0.0.1:0".parse().unwrap()).unwrap();
        let connection = connect(&client, server_addr, "127.0.0.1").await.unwrap();

        let (mut send, mut recv) = connection.open_bi().await.unwrap();
        write_frame(&mut send, &ControlMessage::RequestChunk { index: 5 })
            .await
            .unwrap();

        let response = read_frame(&mut recv).await.unwrap().unwrap();
        match response {
            ControlMessage::Chunk { index, data } => {
                assert_eq!(index, 5);
                assert_eq!(data, b"hello from server");
            }
            other => panic!("意料之外的消息: {other:?}"),
        }

        connection.close(0u32.into(), b"bye");
        client.wait_idle().await;
        server_task.await.unwrap();
    }
}
