//! The sole inbound BI queue owner for a unified node connection.

use std::time::Duration;

use tokio::sync::mpsc;
use tokio::task::{JoinHandle, JoinSet};

use veytrane_transport::error::{Error, Result};

use crate::capability;
use crate::wire::StreamKind;

pub const QUEUE_CAPACITY: usize = 32;
pub const MAX_PENDING_HEADERS: usize = 64;
pub const RESET_BUSY: u32 = 0x101;
pub const RESET_UNSUPPORTED: u32 = 0x102;
pub const RESET_INVALID: u32 = 0x103;
pub const RESET_UNAUTHORIZED: u32 = 0x105;

pub struct BiStream {
    pub send: quinn::SendStream,
    pub recv: quinn::RecvStream,
}
impl BiStream {
    fn reject(mut self, code: u32) {
        let _ = self.send.reset(code.into());
        let _ = self.recv.stop(code.into());
    }
}

pub struct Incoming {
    pub files: mpsc::Receiver<BiStream>,
    pub services: mpsc::Receiver<BiStream>,
    pub speed: mpsc::Receiver<BiStream>,
}

pub struct Dispatcher {
    worker: JoinHandle<()>,
}
impl Drop for Dispatcher {
    fn drop(&mut self) {
        self.worker.abort();
    }
}
impl Dispatcher {
    pub fn start(connection: quinn::Connection, capabilities: u64) -> (Self, Incoming) {
        let (files, file_rx) = mpsc::channel(QUEUE_CAPACITY);
        let (services, service_rx) = mpsc::channel(QUEUE_CAPACITY);
        let (speed, speed_rx) = mpsc::channel(QUEUE_CAPACITY);
        let worker = tokio::spawn(async move {
            let mut headers = JoinSet::new();
            loop {
                tokio::select! {
                    accepted = connection.accept_bi() => {
                        let Ok((send, recv)) = accepted else { break };
                        let stream = BiStream { send, recv };
                        if headers.len() >= MAX_PENDING_HEADERS { stream.reject(RESET_BUSY); continue }
                        headers.spawn(async move {
                            let mut stream = stream;
                            let mut kind = [0];
                            match tokio::time::timeout(Duration::from_secs(5), stream.recv.read_exact(&mut kind)).await {
                                Ok(Ok(())) => match StreamKind::try_from(kind[0]) {
                                    Ok(kind) => Some((kind, stream)),
                                    Err(_) => { stream.reject(RESET_INVALID); None }
                                },
                                _ => { stream.reject(RESET_INVALID); None }
                            }
                        });
                    }
                    result = headers.join_next(), if !headers.is_empty() => {
                        let Some(Ok(Some((kind, stream)))) = result else { continue };
                        let (enabled, destination) = match kind {
                            StreamKind::File => (capabilities & capability::CAP_FILE_TRANSFER != 0, &files),
                            StreamKind::Service => (capabilities & capability::CAP_TCP_TUNNEL != 0, &services),
                            StreamKind::Speed => (capabilities & capability::CAP_SPEED_EXECUTION != 0, &speed),
                        };
                        if !enabled { stream.reject(RESET_UNSUPPORTED); continue }
                        if let Err(error) = destination.try_send(stream) {
                            match error {
                                mpsc::error::TrySendError::Full(stream) => stream.reject(RESET_BUSY),
                                mpsc::error::TrySendError::Closed(stream) => stream.reject(RESET_UNSUPPORTED),
                            }
                        }
                    }
                }
            }
        });
        (
            Self { worker },
            Incoming {
                files: file_rx,
                services: service_rx,
                speed: speed_rx,
            },
        )
    }
    pub fn is_finished(&self) -> bool {
        self.worker.is_finished()
    }
}

/// Open a typed stream on an authenticated node connection.
pub async fn open_bi(connection: &quinn::Connection, kind: StreamKind) -> Result<BiStream> {
    let (mut send, recv) = connection
        .open_bi()
        .await
        .map_err(|error| Error::Transport(format!("打开节点业务流失败：{error}")))?;
    send.write_all(&[kind as u8])
        .await
        .map_err(|error| Error::Transport(format!("写入节点流类型失败：{error}")))?;
    Ok(BiStream { send, recv })
}

#[cfg(all(test, feature = "remote-auth"))]
mod tests {
    use super::*;
    use std::time::Duration;
    use veytrane_transport::quic::{client_endpoint, connect, server_endpoint};

    async fn pair() -> (
        quinn::Endpoint,
        quinn::Endpoint,
        quinn::Connection,
        quinn::Connection,
    ) {
        let server = server_endpoint("127.0.0.1:0".parse().unwrap()).unwrap();
        let client = client_endpoint("127.0.0.1:0".parse().unwrap()).unwrap();
        let accepting = async { server.accept().await.unwrap().await.unwrap() };
        let (client_connection, server_connection) = tokio::join!(
            connect(&client, server.local_addr().unwrap(), "localhost"),
            accepting
        );
        (
            client,
            server,
            client_connection.unwrap(),
            server_connection,
        )
    }

    fn all_capabilities() -> u64 {
        capability::CAP_FILE_TRANSFER | capability::CAP_TCP_TUNNEL | capability::CAP_SPEED_EXECUTION
    }

    async fn wait_queue_len(receiver: &mpsc::Receiver<BiStream>, expected: usize) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while receiver.len() < expected {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("queue did not reach {expected} entries"));
    }

    #[tokio::test]
    async fn pending_header_slots_are_bounded_by_the_next_business_stream() {
        let (_client_endpoint, _server_endpoint, client, server) = pair().await;
        let (_dispatcher, _incoming) = Dispatcher::start(server, all_capabilities());
        let mut silent = Vec::new();
        for _ in 0..MAX_PENDING_HEADERS {
            silent.push(client.open_bi().await.unwrap());
        }
        let (mut successor, _successor_recv) = client.open_bi().await.unwrap();
        successor
            .write_all(&[StreamKind::Service as u8])
            .await
            .unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), successor.stopped())
                .await
                .expect("pending header overflow must reject the successor promptly")
                .unwrap(),
            Some(RESET_BUSY.into())
        );
        drop(silent);
    }

    #[tokio::test]
    async fn slow_low_header_times_out_after_a_later_business_stream_opens_it() {
        let (_client_endpoint, _server_endpoint, client, server) = pair().await;
        let (_dispatcher, _incoming) = Dispatcher::start(server, all_capabilities());
        let (silent, _silent_recv) = client.open_bi().await.unwrap();
        let (mut successor, _successor_recv) = client.open_bi().await.unwrap();
        successor
            .write_all(&[StreamKind::Service as u8])
            .await
            .unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(8), silent.stopped())
                .await
                .expect("implicitly opened low stream must still time out")
                .unwrap(),
            Some(RESET_INVALID.into())
        );
    }

    #[tokio::test]
    async fn valid_stream_queue_is_bounded_per_kind_and_reuses_a_freed_slot() {
        let (_client_endpoint, _server_endpoint, client, server) = pair().await;
        let (_dispatcher, mut incoming) = Dispatcher::start(server, all_capabilities());
        let mut queued = Vec::new();
        for _ in 0..QUEUE_CAPACITY {
            queued.push(open_bi(&client, StreamKind::Service).await.unwrap());
            tokio::task::yield_now().await;
        }
        wait_queue_len(&incoming.services, QUEUE_CAPACITY).await;

        let overflow = open_bi(&client, StreamKind::Service).await.unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), overflow.send.stopped())
                .await
                .expect("full service queue must reject promptly")
                .unwrap(),
            Some(RESET_BUSY.into())
        );

        let speed = open_bi(&client, StreamKind::Speed).await.unwrap();
        wait_queue_len(&incoming.speed, 1).await;
        let _ = tokio::time::timeout(Duration::from_secs(2), incoming.speed.recv())
            .await
            .expect("speed queue must remain independent")
            .expect("speed stream must be queued");

        let _ = tokio::time::timeout(Duration::from_secs(2), incoming.services.recv())
            .await
            .expect("service queue must contain the bounded streams")
            .expect("service stream must be queued");
        wait_queue_len(&incoming.services, QUEUE_CAPACITY - 1).await;
        let replacement = open_bi(&client, StreamKind::Service).await.unwrap();
        wait_queue_len(&incoming.services, QUEUE_CAPACITY).await;
        tokio::time::timeout(Duration::from_secs(5), async {
            for _ in 0..QUEUE_CAPACITY {
                incoming
                    .services
                    .recv()
                    .await
                    .expect("replacement service stream must restore a full queue");
            }
        })
        .await
        .expect("a freed service slot must accept one replacement stream");
        drop((queued, overflow, speed, replacement));
    }
}
