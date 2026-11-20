//! TCP stream budgets, listener supervision, and QUIC byte forwarding.

use std::future::Future;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use quinn::{Connection, RecvStream, SendStream};
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, watch};
use tokio::task::JoinSet;
use tracing::{debug, warn};
use veytrane_transport::error::{Error, Result};

/// Maximum concurrent local TCP connections served by one forwarding rule.
pub const DEFAULT_MAX_TUNNEL_STREAMS: usize = 32;

/// Check the serve-side target allowlist before opening its TCP socket.
pub async fn connect_allowed_target(
    target: SocketAddr,
    allowed: &[SocketAddr],
) -> Result<TcpStream> {
    if !allowed.contains(&target) {
        let reason = format!(
            "目标 {target} 不在允许转发的列表里（当前允许：{}）",
            allowed
                .iter()
                .map(|addr| addr.to_string())
                .collect::<Vec<_>>()
                .join(", ")
        );
        warn!(%target, "拒绝了未经授权的转发请求");
        return Err(Error::Protocol(reason));
    }

    TcpStream::connect(target)
        .await
        .map_err(|err| Error::Transport(format!("连接 {target} 失败: {err}")))
}

/// Structured owner of accepted TCP flows, independent from individual listeners.
pub struct ForwardStreamOwner {
    closed: AtomicBool,
    tasks: Mutex<JoinSet<()>>,
    slots: Arc<Semaphore>,
    limit: usize,
}
impl ForwardStreamOwner {
    pub fn new(limit: usize) -> Self {
        Self {
            closed: AtomicBool::new(false),
            tasks: Mutex::new(JoinSet::new()),
            slots: Arc::new(Semaphore::new(limit)),
            limit,
        }
    }
    pub fn active(&self) -> usize {
        self.limit.saturating_sub(self.slots.available_permits())
    }
    /// Reserve the same node TCP budget for a flow supervised by a peer task.
    pub fn try_reserve(&self) -> Option<OwnedSemaphorePermit> {
        if self.closed.load(Ordering::Acquire) {
            return None;
        }
        let permit = self.slots.clone().try_acquire_owned().ok()?;
        if self.closed.load(Ordering::Acquire) {
            return None;
        }
        Some(permit)
    }
    pub fn spawn(&self, task: impl Future<Output = ()> + Send + 'static) -> bool {
        let Some(permit) = self.try_reserve() else {
            return false;
        };
        let mut tasks = self.tasks.lock().unwrap();
        if self.closed.load(Ordering::Acquire) {
            return false;
        }
        while tasks.try_join_next().is_some() {}
        tasks.spawn(async move {
            let _permit = permit;
            task.await;
        });
        true
    }
    pub async fn shutdown(&self) {
        self.closed.store(true, Ordering::Release);
        let mut tasks = {
            let mut owned = self.tasks.lock().unwrap();
            std::mem::take(&mut *owned)
        };
        tasks.abort_all();
        while tasks.join_next().await.is_some() {}
    }
}

#[derive(Clone)]
enum ForwardErrors {
    Bounded(mpsc::Sender<String>),
    Legacy(mpsc::UnboundedSender<String>),
}
impl ForwardErrors {
    fn report(&self, error: String) {
        match self {
            Self::Bounded(sender) => {
                let _ = sender.try_send(error);
            }
            Self::Legacy(sender) => {
                let _ = sender.send(error);
            }
        }
    }
}

/// Compatibility helper: stopping the listener also stops its accepted flows.
pub async fn forward_on_authenticated_session<F, Fut>(
    listener: TcpListener,
    target: SocketAddr,
    connections: watch::Receiver<Option<Connection>>,
    shutdown: watch::Receiver<bool>,
    errors: mpsc::UnboundedSender<String>,
    open_stream: F,
) -> Result<()>
where
    F: Fn(TcpStream, Connection, SocketAddr) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<()>> + Send + 'static,
{
    let owner = Arc::new(ForwardStreamOwner::new(DEFAULT_MAX_TUNNEL_STREAMS));
    let result = forward_listener(
        listener,
        target,
        connections,
        shutdown,
        ForwardErrors::Legacy(errors),
        owner.clone(),
        open_stream,
    )
    .await;
    owner.shutdown().await;
    result
}

/// Node lifetime owns accepted flows. Rule deletion only stops new acceptance.
pub async fn forward_with_owner<F, Fut>(
    listener: TcpListener,
    target: SocketAddr,
    connections: watch::Receiver<Option<Connection>>,
    shutdown: watch::Receiver<bool>,
    errors: mpsc::Sender<String>,
    owner: Arc<ForwardStreamOwner>,
    open_stream: F,
) -> Result<()>
where
    F: Fn(TcpStream, Connection, SocketAddr) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<()>> + Send + 'static,
{
    forward_listener(
        listener,
        target,
        connections,
        shutdown,
        ForwardErrors::Bounded(errors),
        owner,
        open_stream,
    )
    .await
}

async fn forward_listener<F, Fut>(
    listener: TcpListener,
    target: SocketAddr,
    connections: watch::Receiver<Option<Connection>>,
    mut shutdown: watch::Receiver<bool>,
    errors: ForwardErrors,
    owner: Arc<ForwardStreamOwner>,
    open_stream: F,
) -> Result<()>
where
    F: Fn(TcpStream, Connection, SocketAddr) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<()>> + Send + 'static,
{
    let actual = listener.local_addr()?;
    let streams = Arc::new(Semaphore::new(DEFAULT_MAX_TUNNEL_STREAMS));
    let open_stream = Arc::new(open_stream);
    loop {
        let accepted = tokio::select! {
            biased;
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() { break }
                continue;
            }
            accepted = listener.accept() => accepted,
        };
        let (tcp, from) = accepted
            .map_err(|err| Error::Transport(format!("接受本地连接 {actual} 失败: {err}")))?;
        let permit = match streams.clone().try_acquire_owned() {
            Ok(permit) => permit,
            Err(_) => {
                errors.report(format!("本机转发 {actual} 的并发连接已达上限"));
                continue;
            }
        };
        let connection = connections
            .borrow()
            .clone()
            .filter(|connection| connection.close_reason().is_none());
        let Some(connection) = connection else {
            errors.report("对端尚未建立认证连接，本地 TCP 已关闭；连接就绪后请重试".into());
            continue;
        };
        let task_errors = errors.clone();
        let open_stream = open_stream.clone();
        if !owner.spawn(async move {
            let _permit = permit;
            if let Err(error) = open_stream(tcp, connection, target).await {
                task_errors.report(error.to_string());
                debug!(%from, %target, %error, "本机转发连接失败");
            }
        }) {
            errors.report("节点 TCP 业务流并发已达上限".into());
        }
    }
    // listener drops here; accepted work remains in the node's structured owner.
    Ok(())
}

/// 把一条 TCP 连接和一条 QUIC 双向流对接起来，双向搬字节。
pub async fn bridge_tcp(
    tcp: TcpStream,
    mut quic_send: SendStream,
    mut quic_recv: RecvStream,
) -> Result<()> {
    let (mut tcp_read, mut tcp_write) = tcp.into_split();

    // 上行：TCP → QUIC。
    let up = async {
        let copied = tokio::io::copy(&mut tcp_read, &mut quic_send).await?;
        // 本端读完就把写方向关掉，让对端看到 EOF（半关闭语义）。
        let _ = quic_send.finish();
        Ok::<u64, Error>(copied)
    };

    // 下行：QUIC → TCP。
    let down = async {
        let copied = tokio::io::copy(&mut quic_recv, &mut tcp_write).await?;
        let _ = tcp_write.shutdown().await;
        Ok::<u64, Error>(copied)
    };

    // 两个方向都跑完才算这条隧道结束。
    let (up_bytes, down_bytes) = tokio::try_join!(up, down)?;
    tracing::debug!(up_bytes, down_bytes, "隧道关闭");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn reserved_service_flows_share_the_forward_budget_and_release_on_shutdown() {
        let owner = ForwardStreamOwner::new(2);
        let inbound = owner.try_reserve().unwrap();
        assert!(owner.spawn(std::future::pending::<()>()));
        assert_eq!(owner.active(), 2);
        assert!(owner.try_reserve().is_none());
        assert!(!owner.spawn(std::future::pending::<()>()));
        drop(inbound);
        let inbound = owner.try_reserve().unwrap();
        owner.shutdown().await;
        assert!(owner.try_reserve().is_none());
        drop(inbound);
        assert_eq!(owner.active(), 0);
    }
}
