//! 节点发现：局域网组播、公网信令。

pub mod mdns;
pub mod signal;
pub mod signal_tls;

pub use mdns::{
    LanDiscovery, PeerAnnouncement, SERVICE_TYPE, local_ip_addresses, parse_announcement,
};
pub use signal::{
    Candidate, CandidateKind, DEFAULT_SIGNAL_PORT, LookupOutcome, PeerOffer, SignalMessage,
    SignalingClient, dedup_candidates, sort_candidates,
};
#[cfg(any(feature = "signaling-server", test))]
pub use signal::{run_signal_server, run_signal_server_on};

pub mod short_id;
#[cfg(any(feature = "signaling-server", test))]
#[path = "../../apps/signaling/src/short_id_store.rs"]
mod short_id_store;
