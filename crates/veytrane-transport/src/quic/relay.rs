//! Relay wire and one-address UDP admission/authentication primitives.

pub mod wire;

use crate::connection::PreparedTransport;
use crate::error::{Error, Result};
use crate::identity::{Identity, NodeId};
use crate::punch::PunchToken;
use crate::quic::endpoint_from_socket_with_config;
use crate::udp::{AddressFamily, bind_udp};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::UdpSocket;
use wire::{Challenge, Hello, Packet};

pub const RELAY_BIND_TIMEOUT: Duration = Duration::from_secs(8);
pub const RELAY_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(18);

pub type Progress = Arc<dyn Fn(&str) + Send + Sync>;

/// No socket is shared with direct paths or another peer/family attempt.
pub async fn bind(
    identity: &Identity,
    peer: NodeId,
    token: PunchToken,
    server: SocketAddr,
) -> Result<quinn::Endpoint> {
    bind_with_progress(identity, peer, token, server, None).await
}

async fn bind_with_progress(
    identity: &Identity,
    peer: NodeId,
    token: PunchToken,
    server: SocketAddr,
    progress: Option<&Progress>,
) -> Result<quinn::Endpoint> {
    let family = AddressFamily::of(server);
    if !family.accepts(server) {
        return Err(Error::Transport("invalid Relay UDP address".into()));
    }
    let socket = UdpSocket::from_std(bind_udp(family.wildcard(0))?)?;
    let local = socket.local_addr()?;
    let hello = Hello::new(identity, peer, token);
    let hello_bytes = wire::encode(Packet::Hello(hello))?;
    let mut retry = tokio::time::interval(Duration::from_millis(200));
    // Windows reports WSAEMSGSIZE for oversized receives into a short buffer.
    // Receive the full bounded UDP datagram and then apply the control limit.
    let mut buffer = [0_u8; 65_536];
    let deadline = tokio::time::sleep(RELAY_BIND_TIMEOUT);
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            _ = &mut deadline => return Err(Error::Transport(format!("Relay {family} UDP admission timeout: {server}"))),
            _ = retry.tick() => { socket.send_to(&hello_bytes, server).await?; },
            received = socket.recv_from(&mut buffer) => {
                let (n, from) = received?;
                if from != server || n > wire::MAX_CONTROL { continue; }
                match wire::decode(&buffer[..n]) {
                    Ok(Packet::Challenge {client_nonce, nonce, source}) if client_nonce == hello.client_nonce && family.accepts(source) => {
                        let challenge = Challenge { hello, nonce, source };
                        socket.send_to(&wire::encode(wire::register(identity, challenge)?)?, server).await?;
                    },
                    Ok(Packet::Ready {token: ready_token, node, client_nonce}) if ready_token == token && node == identity.node_id() && client_nonce == hello.client_nonce => break,
                    _ => {},
                }
            }
        }
    }
    // RelayReady is UDP readiness only, never peer authentication.
    tracing::info!(%server, %family, %local, "Relay UDP 已就绪；等待端到端 QUIC 身份认证");
    if let Some(progress) = progress {
        progress(&format!(
            "Relay UDP {family} 已就绪；等待端到端 QUIC 身份认证"
        ));
    }
    // Keep the admission magic disjoint from these endpoints' QUIC headers.
    // Quinn defaults to accepting/advertising fixed-bit greasing (RFC 9287).
    // Direct paths keep their existing default; both Relay peers opt out.
    let mut endpoint_config = quinn::EndpointConfig::default();
    endpoint_config.grease_quic_bit(false);
    let endpoint = endpoint_from_socket_with_config(socket.into_std()?, endpoint_config)?;
    if endpoint.local_addr()? != local {
        return Err(Error::Transport(
            "Relay socket changed at Quinn handoff".into(),
        ));
    }
    Ok(endpoint)
}

pub async fn prepare(
    identity: &Identity,
    peer: NodeId,
    token: PunchToken,
    server: SocketAddr,
) -> Result<PreparedTransport> {
    prepare_with_progress(identity, peer, token, server, None).await
}

pub async fn prepare_with_progress(
    identity: &Identity,
    peer: NodeId,
    token: PunchToken,
    server: SocketAddr,
    progress: Option<Progress>,
) -> Result<PreparedTransport> {
    tokio::time::timeout(RELAY_ATTEMPT_TIMEOUT, async {
        let endpoint = bind_with_progress(identity, peer, token, server, progress.as_ref()).await?;
        // Same deterministic NodeId rule as Desktop direct paths. Application
        // client/server roles are independent of who initiates the QUIC handshake.
        let mut prepared = if identity.node_id() < peer {
            PreparedTransport::dial(&endpoint, server, identity, peer).await?
        } else {
            let incoming = endpoint
                .accept()
                .await
                .ok_or_else(|| Error::Transport("Relay endpoint closed".into()))?;
            if incoming.remote_address() != server {
                incoming.refuse();
                return Err(Error::Transport("Relay source changed".into()));
            }
            PreparedTransport::accept(incoming, identity, peer).await?
        };
        prepared.own_relay_endpoint(endpoint);
        Ok(prepared)
    })
    .await
    .map_err(|_| Error::Transport(format!("Relay {server} authenticated attempt timeout")))?
}
