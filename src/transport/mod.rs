//! 数据通道：基于 QUIC 的加密多路复用连接。

#[cfg(feature = "agent-base")]
pub mod diagnostic;
pub mod handshake;
pub mod identity_wire;
pub mod quic;

pub use handshake::{HandshakeOutcome, handshake_initiator, handshake_responder};
pub use identity_wire::{
    HANDSHAKE_DOMAIN, IDENTITY_PROTOCOL_VERSION, IdentityMessage, handshake_payload,
};
pub use quic::{
    ACCEPT_FIRST_BI_STREAM_TIMEOUT, ALPN, APPLICATION_HANDSHAKE_TIMEOUT, CHANNEL_BINDING_LABEL,
    CHANNEL_BINDING_LEN, ChannelBinding, KEEP_ALIVE_INTERVAL, MAX_IDLE_TIMEOUT,
    QUIC_HANDSHAKE_TIMEOUT, STREAM_FIRST_FRAME_TIMEOUT, TRANSFER_IDLE_TIMEOUT, client_endpoint,
    connect, endpoint_from_socket, install_crypto_provider, server_endpoint,
};
