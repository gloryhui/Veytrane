use serde::{Deserialize, Serialize};
use veytrane_transport::identity::NodeId;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustedDevice {
    pub node_id: String,
    pub display_name: String,
    pub last_short_id: Option<String>,
    pub trusted_at: u64,
    pub updated_at: u64,
}

impl TrustedDevice {
    pub fn new(peer: NodeId, display_name: String, last_short_id: Option<String>) -> Self {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        Self {
            node_id: peer.to_hex(),
            display_name,
            last_short_id,
            trusted_at: now,
            updated_at: now,
        }
    }
}
