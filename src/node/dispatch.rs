//! The sole inbound BI queue owner for a unified node connection.

use std::time::Duration;

use tokio::sync::mpsc;
use tokio::task::{JoinHandle, JoinSet};

use super::{protocol, wire::StreamKind};
use crate::error::{Error, Result};

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
                            StreamKind::File => (capabilities & protocol::CAP_FILE_TRANSFER != 0, &files),
                            StreamKind::Service => (capabilities & protocol::CAP_TCP_TUNNEL != 0, &services),
                            StreamKind::Speed => (capabilities & protocol::CAP_SPEED_EXECUTION != 0, &speed),
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

/// Explicit framing mode supplied by the authenticated connection owner.
pub async fn open_bi(
    connection: &quinn::Connection,
    kind: StreamKind,
    unified: bool,
) -> Result<BiStream> {
    let (mut send, recv) = connection
        .open_bi()
        .await
        .map_err(|error| Error::Transport(format!("打开节点业务流失败：{error}")))?;
    if unified {
        send.write_all(&[kind as u8])
            .await
            .map_err(|error| Error::Transport(format!("写入节点流类型失败：{error}")))?;
    }
    Ok(BiStream { send, recv })
}
