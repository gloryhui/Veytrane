//! Compatibility path for the identity handshake wire.

pub use veytrane_transport::identity_wire::{
    HANDSHAKE_DOMAIN, IDENTITY_PROTOCOL_VERSION, IdentityMessage, handshake_payload,
    read_identity_frame, write_identity_frame,
};

#[cfg(all(test, feature = "standalone"))]
mod tests {
    use super::*;
    use crate::identity::NodeId;
    use crate::protocol::message::{ControlMessage, PROTOCOL_VERSION};

    fn hello(node_id: NodeId) -> IdentityMessage {
        IdentityMessage::Hello {
            protocol_version: IDENTITY_PROTOCOL_VERSION,
            node_id,
            public_key: [0x22; 32],
            nonce: [0x33; 32],
        }
    }

    #[test]
    fn identity_bytes_match_control_message_identity_discriminants() {
        let node_id = NodeId::from_hex("11111111111111111111111111111111").unwrap();
        assert_eq!(
            hello(node_id).encode().unwrap(),
            ControlMessage::Hello {
                protocol_version: PROTOCOL_VERSION,
                node_id,
                public_key: [0x22; 32],
                nonce: [0x33; 32],
            }
            .encode()
            .unwrap()
        );
        assert!(IdentityMessage::decode(&ControlMessage::Bye.encode().unwrap()).is_err());
        assert_eq!(IDENTITY_PROTOCOL_VERSION, PROTOCOL_VERSION);
    }
}
