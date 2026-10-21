//! Local resource policy and provenance; neither implies a password proof.
use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::identity::NodeId;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TrustSource {
    #[default]
    LegacyImport,
    PasswordVerified,
    Administrator,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PeerResources {
    pub node_id: String,
    #[serde(default)]
    pub files: bool,
    #[serde(default)]
    pub speed: bool,
    #[serde(default)]
    pub disabled: bool,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResourcePolicy {
    #[serde(default)]
    pub peers: Vec<PeerResources>,
}

impl ResourcePolicy {
    pub fn validate(&self) -> Result<()> {
        if self.peers.len() > 128 {
            return Err(Error::Protocol("资源策略最多 128 台设备".into()));
        }
        let mut ids = HashSet::new();
        for entry in &self.peers {
            let peer = NodeId::from_hex(&entry.node_id)?;
            if entry.node_id != peer.to_hex() || !ids.insert(peer) {
                return Err(Error::Protocol("资源策略设备 ID 非规范或重复".into()));
            }
        }
        Ok(())
    }
    pub fn for_peer(&self, peer: NodeId) -> PeerResources {
        let id = peer.to_hex();
        self.peers
            .iter()
            .find(|entry| entry.node_id == id)
            .cloned()
            .unwrap_or_else(|| PeerResources {
                node_id: peer.to_hex(),
                ..Default::default()
            })
    }
}
