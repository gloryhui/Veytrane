//! Recover signaling independently from authenticated business connections.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::{info, warn};

use crate::discovery::signal::{Candidate, SignalMessage, SignalingClient};
use crate::discovery::signal_tls::SignalTlsClient;
use crate::identity::{Identity, NodeId};

struct Event {
    generation: u64,
    message: SignalMessage,
}

/// A single reader and reconnect owner. Dropping this handle cancels all work.
pub(crate) struct ResilientSignaling {
    events: mpsc::Receiver<Event>,
    generation: Arc<AtomicU64>,
    worker: JoinHandle<()>,
}

impl ResilientSignaling {
    pub(crate) fn spawn(
        initial: SignalingClient,
        server: String,
        tls: Option<SignalTlsClient>,
        identity: Identity,
        peer: NodeId,
        candidates: Vec<Candidate>,
    ) -> Self {
        let (sender, events) = mpsc::channel(64);
        let generation = Arc::new(AtomicU64::new(1));
        let current = generation.clone();
        let worker = tokio::spawn(async move {
            let mut client = Some(initial);
            let mut delay = Duration::from_secs(1);
            loop {
                if let Some(active) = client.as_mut() {
                    match active.next_event().await {
                        Ok(message) => {
                            let event = Event {
                                generation: current.load(Ordering::Acquire),
                                message,
                            };
                            if sender.send(event).await.is_err() {
                                return;
                            }
                            continue;
                        }
                        Err(error) => {
                            warn!(%error, "信令失联，保留认证业务连接并独立重连");
                            current.fetch_add(1, Ordering::AcqRel);
                            // Drop reader/writer before starting a new registration.
                            client = None;
                        }
                    }
                }
                tokio::time::sleep(delay).await;
                match SignalingClient::connect_with_tls(
                    &server,
                    &identity,
                    candidates.clone(),
                    tls.as_ref(),
                )
                .await
                {
                    Ok(registered) => {
                        if let Err(error) = registered.request_lookup(peer).await {
                            warn!(%error, "信令重登后的对端查询失败，稍后重新登记");
                            delay = (delay * 2).min(Duration::from_secs(30));
                            continue;
                        }
                        info!("信令已使用原身份重新登记，现有业务连接保持运行");
                        client = Some(registered);
                        delay = Duration::from_secs(1);
                    }
                    Err(error) => {
                        warn!(%error, ?delay, "信令重登失败");
                        delay = (delay * 2).min(Duration::from_secs(30));
                    }
                }
            }
        });
        Self {
            events,
            generation,
            worker,
        }
    }

    pub(crate) async fn next_event(&mut self) -> Option<SignalMessage> {
        while let Some(event) = self.events.recv().await {
            if event.generation == self.generation.load(Ordering::Acquire) {
                return Some(event.message);
            }
        }
        None
    }
}

impl Drop for ResilientSignaling {
    fn drop(&mut self) {
        self.worker.abort();
    }
}
