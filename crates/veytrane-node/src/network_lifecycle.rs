//! Network-wide lifecycle states for an assembled node.

use std::time::Duration;

use veytrane_transport::identity::NodeId;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NetworkLifecycle {
    Unconfigured,
    LocalOnly,
    ConnectingSignal,
    SignalOnline,
    ReconnectingSignal {
        attempt: u32,
        delay: Duration,
    },
    PeerPending {
        peer: NodeId,
    },
    Punching {
        peer: NodeId,
    },
    Authenticating {
        peer: NodeId,
    },
    Connected {
        peer: NodeId,
    },
    Disconnected {
        peer: Option<NodeId>,
        detail: String,
    },
    Failed {
        detail: String,
    },
}
