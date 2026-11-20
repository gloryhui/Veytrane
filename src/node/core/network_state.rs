//! Network-wide session status and peer registry compatibility paths.

pub use veytrane_node::network_lifecycle::NetworkLifecycle;
pub use veytrane_node::peer_registry::{
    BeginPeerAttempt, DEFAULT_MAX_PEERS, MAX_PEER_CAPACITY, MAX_PENDING_PEERS, PEER_LOOKUP_TIMEOUT,
    PeerLifecycle, PeerRegistry, reconnect_delay, should_initiate_quic,
};
