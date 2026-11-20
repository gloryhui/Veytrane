//! QUIC identity-handshake wire: Hello, HelloAck, Auth and Ready only.
//!
//! The standalone CLI keeps its wider `ControlMessage`. This module uses the same
//! v3 postcard discriminants for the four identity messages, but never decodes the
//! file, tunnel or speed messages.

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncWrite};

use crate::error::{Error, Result};
use crate::identity::NodeId;
use crate::raw_frame::{MAX_FRAME_LEN, read_raw_frame, write_raw_frame};

/// Identity handshake protocol version.
pub const IDENTITY_PROTOCOL_VERSION: u32 = 3;
/// Signature domain label for the v3 identity handshake.
pub const HANDSHAKE_DOMAIN: &[u8] = b"p2p_file/handshake/v3";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum IdentityMessage {
    Hello {
        protocol_version: u32,
        node_id: NodeId,
        public_key: [u8; 32],
        nonce: [u8; 32],
    },
    HelloAck {
        protocol_version: u32,
        node_id: NodeId,
        public_key: [u8; 32],
        nonce: [u8; 32],
        peer_nonce: [u8; 32],
        signature: Vec<u8>,
    },
    Auth {
        signature: Vec<u8>,
    },
    Ready,
}

impl IdentityMessage {
    pub fn encode(&self) -> Result<Vec<u8>> {
        Ok(postcard::to_allocvec(self)?)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        Ok(postcard::from_bytes(bytes)?)
    }

    pub fn kind(&self) -> &'static str {
        match self {
            Self::Hello { .. } => "Hello",
            Self::HelloAck { .. } => "HelloAck",
            Self::Auth { .. } => "Auth",
            Self::Ready => "Ready",
        }
    }
}

pub async fn write_identity_frame<W>(writer: &mut W, message: &IdentityMessage) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    let payload = message.encode()?;
    if payload.len() > MAX_FRAME_LEN as usize {
        return Err(Error::Protocol(format!(
            "{} 帧过大：{} 字节，上限 {MAX_FRAME_LEN}",
            message.kind(),
            payload.len()
        )));
    }
    write_raw_frame(writer, &payload).await
}

pub async fn read_identity_frame<R>(reader: &mut R) -> Result<Option<IdentityMessage>>
where
    R: AsyncRead + Unpin,
{
    match read_raw_frame(reader).await? {
        Some(payload) => Ok(Some(IdentityMessage::decode(&payload)?)),
        None => Ok(None),
    }
}

/// Role-independent payload signed by both identity handshake participants.
pub fn handshake_payload(
    public_key_a: &[u8; 32],
    nonce_a: &[u8; 32],
    public_key_b: &[u8; 32],
    nonce_b: &[u8; 32],
    channel_binding: &[u8; 32],
) -> Vec<u8> {
    let (first, second) = if public_key_a <= public_key_b {
        ((public_key_a, nonce_a), (public_key_b, nonce_b))
    } else {
        ((public_key_b, nonce_b), (public_key_a, nonce_a))
    };

    let mut payload = Vec::with_capacity(HANDSHAKE_DOMAIN.len() + 2 * (32 + 32) + 32);
    payload.extend_from_slice(HANDSHAKE_DOMAIN);
    payload.extend_from_slice(first.0);
    payload.extend_from_slice(first.1);
    payload.extend_from_slice(second.0);
    payload.extend_from_slice(second.1);
    payload.extend_from_slice(channel_binding);
    payload
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::{Identity, signature_from_bytes};

    fn hello(node_id: NodeId) -> IdentityMessage {
        IdentityMessage::Hello {
            protocol_version: IDENTITY_PROTOCOL_VERSION,
            node_id,
            public_key: [0x22; 32],
            nonce: [0x33; 32],
        }
    }

    #[test]
    fn fixed_v3_identity_bytes_match_s0_vectors() {
        let node_id = NodeId::from_hex("11111111111111111111111111111111").unwrap();
        let hello = hello(node_id);
        let mut hello_bytes = vec![0, 3];
        hello_bytes.extend_from_slice(&[0x11; 16]);
        hello_bytes.extend_from_slice(&[0x22; 32]);
        hello_bytes.extend_from_slice(&[0x33; 32]);

        let ack = IdentityMessage::HelloAck {
            protocol_version: 3,
            node_id,
            public_key: [0x22; 32],
            nonce: [0x33; 32],
            peer_nonce: [0x44; 32],
            signature: vec![0x55; 64],
        };
        let mut ack_bytes = hello_bytes.clone();
        ack_bytes[0] = 1;
        ack_bytes.extend_from_slice(&[0x44; 32]);
        ack_bytes.push(64);
        ack_bytes.extend_from_slice(&[0x55; 64]);

        let auth = IdentityMessage::Auth {
            signature: vec![0x55; 64],
        };
        let mut auth_bytes = vec![2, 64];
        auth_bytes.extend_from_slice(&[0x55; 64]);

        for (message, bytes) in [
            (hello, hello_bytes),
            (ack, ack_bytes),
            (auth, auth_bytes),
            (IdentityMessage::Ready, vec![3]),
        ] {
            assert_eq!(message.encode().unwrap(), bytes);
            assert_eq!(IdentityMessage::decode(&bytes).unwrap(), message);
        }
    }

    #[tokio::test]
    async fn identity_frame_roundtrip_uses_the_neutral_raw_frame_layer() {
        let (mut writer, mut reader) = tokio::io::duplex(4096);
        let message = IdentityMessage::Auth {
            signature: vec![0x5a; 64],
        };
        let outgoing = message.clone();
        let task = tokio::spawn(async move { write_identity_frame(&mut writer, &outgoing).await });
        assert_eq!(
            read_identity_frame(&mut reader).await.unwrap(),
            Some(message)
        );
        task.await.unwrap().unwrap();
    }

    #[test]
    fn signature_domain_and_payload_match_s0_vectors() {
        let mut signing_bytes = b"p2p_file/handshake/v3".to_vec();
        for block in [0x22, 0x33, 0x66, 0x44, 0x77] {
            signing_bytes.extend_from_slice(&[block; 32]);
        }
        assert_eq!(
            handshake_payload(
                &[0x66; 32],
                &[0x44; 32],
                &[0x22; 32],
                &[0x33; 32],
                &[0x77; 32]
            ),
            signing_bytes
        );
    }

    #[test]
    fn handshake_payload_is_role_independent_and_binding_sensitive() {
        let alice = Identity::generate();
        let bob = Identity::generate();
        let nonce_a = [0xaa; 32];
        let nonce_b = [0xbb; 32];
        let binding = [0xcc; 32];

        let from_alice = handshake_payload(
            &alice.public_key_bytes(),
            &nonce_a,
            &bob.public_key_bytes(),
            &nonce_b,
            &binding,
        );
        let from_bob = handshake_payload(
            &bob.public_key_bytes(),
            &nonce_b,
            &alice.public_key_bytes(),
            &nonce_a,
            &binding,
        );
        assert_eq!(from_alice, from_bob);

        let mut flipped = binding;
        flipped[7] ^= 0x01;
        assert_ne!(
            from_alice,
            handshake_payload(
                &alice.public_key_bytes(),
                &nonce_a,
                &bob.public_key_bytes(),
                &nonce_b,
                &flipped,
            )
        );

        let signature = alice.sign(&from_alice);
        assert!(
            alice
                .verify(
                    &from_alice,
                    &signature_from_bytes(&signature.to_bytes()).unwrap()
                )
                .is_ok()
        );
    }
}
