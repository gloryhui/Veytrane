//! Authenticated QUIC connections and retained endpoint ownership.

use crate::channel_binding::ChannelBinding;
use crate::error::{Error, Result};
use crate::handshake::{
    APPLICATION_HANDSHAKE_TIMEOUT, prepare_handshake_initiator, prepare_handshake_responder,
};
use crate::identity::{Identity, NodeId};
use crate::quic::connect;
use crate::udp::AddressFamily;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

/// Only the selected racing initiator sends FIN after Ready. The responder
/// waits for it before exposing capabilities, authorization or handlers.
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

/// Owned by the Desktop network or CLI link. Only winning endpoints are retained;
/// pending/losing endpoints stay inside cancellation guards. No detached keepalive task.
#[derive(Clone, Default)]
pub struct EndpointPool(Arc<Mutex<PoolState>>);

#[derive(Default)]
struct PoolState {
    closed: bool,
    endpoints: Vec<(usize, quinn::Endpoint)>,
}

impl EndpointPool {
    pub(crate) fn retain(
        &self,
        connection: &quinn::Connection,
        endpoint: quinn::Endpoint,
    ) -> Result<()> {
        let mut endpoints = self
            .0
            .lock()
            .map_err(|_| Error::Transport("Relay endpoint ownership unavailable".into()))?;
        if endpoints.closed {
            return Err(Error::Transport("Relay network already closed".into()));
        }
        endpoints
            .endpoints
            .retain(|(_, endpoint)| endpoint.open_connections() > 0);
        if endpoints.endpoints.len() >= 64 {
            return Err(Error::Transport("Relay endpoint resource limit".into()));
        }
        endpoints.endpoints.push((connection.stable_id(), endpoint));
        Ok(())
    }
    pub fn is_relay(&self, connection: &quinn::Connection) -> bool {
        self.0.lock().is_ok_and(|items| {
            items
                .endpoints
                .iter()
                .any(|(id, _)| *id == connection.stable_id())
        })
    }
    pub fn endpoints(&self) -> Vec<quinn::Endpoint> {
        self.0
            .lock()
            .map(|items| {
                items
                    .endpoints
                    .iter()
                    .map(|(_, endpoint)| endpoint.clone())
                    .collect()
            })
            .unwrap_or_default()
    }
    pub fn close(&self) {
        if let Ok(mut state) = self.0.lock() {
            state.closed = true;
            for (_, endpoint) in &state.endpoints {
                endpoint.close(0_u32.into(), b"Relay network shutdown");
            }
        }
    }
}

/// Close on errors, task cancellation and losers, even if Quinn has other handles.
pub struct ConnectionGuard {
    connection: Option<quinn::Connection>,
    relay_endpoint: Option<quinn::Endpoint>,
}

impl ConnectionGuard {
    pub fn new(connection: quinn::Connection) -> Self {
        Self {
            connection: Some(connection),
            relay_endpoint: None,
        }
    }
    pub fn connection(&self) -> &quinn::Connection {
        self.connection.as_ref().unwrap()
    }
    pub fn release(mut self) -> quinn::Connection {
        assert!(
            self.relay_endpoint.is_none(),
            "Relay endpoint ownership must be retained before releasing its connection"
        );
        self.connection.take().unwrap()
    }
    pub fn own_relay_endpoint(&mut self, endpoint: quinn::Endpoint) {
        self.relay_endpoint = Some(endpoint);
    }
    pub fn retain_relay_endpoint(&mut self, pool: &EndpointPool) -> Result<()> {
        if let Some(endpoint) = &self.relay_endpoint {
            pool.retain(self.connection(), endpoint.clone())?;
        }
        self.relay_endpoint.take();
        Ok(())
    }
    pub fn is_relay(&self) -> bool {
        self.relay_endpoint.is_some()
    }
}

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        if let Some(connection) = &self.connection {
            connection.close(0u32.into(), b"unselected or cancelled transport");
        }
    }
}

/// QUIC, expected NodeId, both Ed25519 signatures, TLS binding and Ready are verified.
/// The dedicated stream is kept open until this candidate wins selection.
pub struct PreparedTransport {
    guard: ConnectionGuard,
    send: quinn::SendStream,
    _recv: quinn::RecvStream,
}

impl PreparedTransport {
    pub async fn accept(
        incoming: quinn::Incoming,
        identity: &Identity,
        expected: NodeId,
    ) -> Result<Self> {
        let (_, prepared) = Self::accept_checked(incoming, identity, Some(expected)).await?;
        Ok(prepared)
    }
    /// Identity is verified before it is returned. This does not grant resources.
    pub async fn accept_identified(
        incoming: quinn::Incoming,
        identity: &Identity,
    ) -> Result<(NodeId, Self)> {
        Self::accept_checked(incoming, identity, None).await
    }
    async fn accept_checked(
        incoming: quinn::Incoming,
        identity: &Identity,
        expected: Option<NodeId>,
    ) -> Result<(NodeId, Self)> {
        let connection = incoming
            .await
            .map_err(|e| Error::Transport(e.to_string()))?;
        let guard = ConnectionGuard::new(connection);
        let binding = ChannelBinding::from_connection(guard.connection())?;
        let (mut send, mut recv) = guard
            .connection()
            .accept_bi()
            .await
            .map_err(|e| Error::Transport(e.to_string()))?;
        let verified =
            prepare_handshake_responder(&mut send, &mut recv, identity, &binding).await?;
        let peer = verified.outcome().peer_node_id;
        if expected.is_some_and(|id| id != peer) {
            return Err(Error::Identity("unexpected Relay peer NodeId".into()));
        }
        verified.finish(&mut send).await?;
        Ok((
            peer,
            Self {
                guard,
                send,
                _recv: recv,
            },
        ))
    }
    pub fn own_relay_endpoint(&mut self, endpoint: quinn::Endpoint) {
        self.guard.own_relay_endpoint(endpoint);
    }
    /// The peer selects among its candidates. FIN is the selection commit, not
    /// RelayReady; this also supports CLI application roles independent of QUIC roles.
    pub async fn wait_for_selection(mut self) -> Result<ConnectionGuard> {
        self.send
            .finish()
            .map_err(|e| Error::Transport(e.to_string()))?;
        await_identity_commit(&mut self._recv).await?;
        Ok(self.guard)
    }
    pub async fn dial(
        endpoint: &quinn::Endpoint,
        remote: SocketAddr,
        identity: &Identity,
        expected: NodeId,
    ) -> Result<Self> {
        if !AddressFamily::of(endpoint.local_addr()?).accepts(remote) {
            return Err(Error::Transport(
                "QUIC candidate/path family mismatch".into(),
            ));
        }
        let connection = connect(endpoint, remote, "p2pfile").await?;
        let guard = ConnectionGuard::new(connection);
        let binding = ChannelBinding::from_connection(guard.connection())?;
        let (mut send, mut recv) = guard
            .connection()
            .open_bi()
            .await
            .map_err(|e| Error::Transport(e.to_string()))?;
        let handshake =
            prepare_handshake_initiator(&mut send, &mut recv, identity, &binding).await?;
        if handshake.outcome.peer_node_id != expected {
            return Err(Error::Identity(format!(
                "unexpected peer: expected {expected}, got {}",
                handshake.outcome.peer_node_id
            )));
        }
        // Ready proves that the responder verified our Auth on this binding.
        handshake.finish(&mut send, &mut recv).await?;
        Ok(Self {
            guard,
            send,
            _recv: recv,
        })
    }
    pub async fn finish(self) -> Result<ConnectionGuard> {
        let Self {
            guard,
            mut send,
            _recv,
        } = self;
        // Only the selected, fully authenticated path sends FIN. Responders wait
        // for it before capabilities, Remote Auth, or business dispatch.
        send.finish().map_err(|e| Error::Transport(e.to_string()))?;
        Ok(guard)
    }
}
