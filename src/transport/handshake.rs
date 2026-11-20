//! Compatibility path for the identity handshake plus CLI `ControlMessage` interop tests.

pub use veytrane_transport::handshake::{
    APPLICATION_HANDSHAKE_TIMEOUT, HandshakeOutcome, PreparedHandshake, VerifiedResponder,
    handshake_initiator, handshake_responder, prepare_handshake_initiator,
    prepare_handshake_responder,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::{Error, Result};
    use crate::identity::{
        Identity, NodeId, public_key_from_bytes, signature_from_bytes, verify_signature,
    };
    use crate::protocol::frame::{read_frame, write_frame};
    use crate::protocol::message::{ControlMessage, PROTOCOL_VERSION};
    use crate::transport::identity_wire::{IDENTITY_PROTOCOL_VERSION, handshake_payload};
    use crate::transport::quic::ChannelBinding;
    use std::time::Duration;

    type Half = (
        tokio::io::ReadHalf<tokio::io::DuplexStream>,
        tokio::io::WriteHalf<tokio::io::DuplexStream>,
    );

    fn split_pair() -> (Half, Half) {
        let (a, b) = tokio::io::duplex(64 * 1024);
        (tokio::io::split(a), tokio::io::split(b))
    }

    fn test_binding() -> ChannelBinding {
        ChannelBinding::from_bytes_for_test([0x5a; 32])
    }

    fn check_version(version: u32) -> Result<()> {
        if version == IDENTITY_PROTOCOL_VERSION {
            Ok(())
        } else {
            Err(Error::Protocol(format!(
                "协议版本不兼容：本机 {IDENTITY_PROTOCOL_VERSION}，对端 {version}"
            )))
        }
    }

    fn check_node_id(public_key: &ed25519_dalek::VerifyingKey, claimed: NodeId) -> Result<()> {
        let derived = NodeId::from_public_key(public_key);
        if derived == claimed {
            Ok(())
        } else {
            Err(Error::Protocol(format!(
                "节点 ID 与公钥不符：声称 {}，公钥实为 {}",
                claimed.short(),
                derived.short()
            )))
        }
    }

    async fn delayed_responder(
        send: &mut (impl tokio::io::AsyncWrite + Unpin),
        recv: &mut (impl tokio::io::AsyncRead + Unpin),
        identity: &Identity,
        binding: &ChannelBinding,
        delay: Duration,
    ) -> Result<()> {
        tokio::time::sleep(delay).await;
        let hello = read_frame(recv)
            .await?
            .ok_or_else(|| Error::Protocol("测试 responder 未收到 Hello".into()))?;
        let (peer_nonce, peer_public_key, _peer_node_id) = match hello {
            ControlMessage::Hello {
                protocol_version,
                node_id,
                public_key,
                nonce,
            } => {
                check_version(protocol_version)?;
                let key = public_key_from_bytes(&public_key)?;
                check_node_id(&key, node_id)?;
                (nonce, key, node_id)
            }
            other => {
                return Err(Error::Protocol(format!(
                    "测试 responder 收到 {}",
                    other.kind()
                )));
            }
        };
        let our_nonce: [u8; 32] = rand::random();
        let payload = handshake_payload(
            &peer_public_key.to_bytes(),
            &peer_nonce,
            &identity.public_key_bytes(),
            &our_nonce,
            binding.as_bytes(),
        );
        let signature = identity.sign(&payload);
        write_frame(
            send,
            &ControlMessage::HelloAck {
                protocol_version: PROTOCOL_VERSION,
                node_id: identity.node_id(),
                public_key: identity.public_key_bytes(),
                nonce: our_nonce,
                peer_nonce,
                signature: signature.to_bytes().to_vec(),
            },
        )
        .await?;

        tokio::time::sleep(delay).await;
        let auth = read_frame(recv)
            .await?
            .ok_or_else(|| Error::Protocol("测试 responder 未收到 Auth".into()))?;
        match auth {
            ControlMessage::Auth { signature } => {
                verify_signature(
                    &peer_public_key,
                    &payload,
                    &signature_from_bytes(&signature)?,
                )?;
            }
            other => {
                return Err(Error::Protocol(format!(
                    "测试 responder 收到 {}",
                    other.kind()
                )));
            }
        }
        tokio::time::sleep(delay).await;
        write_frame(send, &ControlMessage::Ready).await
    }

    #[tokio::test(start_paused = true)]
    async fn handshake_stages_have_independent_idle_timeouts() {
        let alice = Identity::generate();
        let bob = Identity::generate();
        let binding = test_binding();
        let ((mut a_recv, mut a_send), (mut b_recv, mut b_send)) = split_pair();
        let initiator = tokio::spawn(async move {
            handshake_initiator(&mut a_send, &mut a_recv, &alice, &binding).await
        });
        let responder = tokio::spawn(async move {
            delayed_responder(
                &mut b_send,
                &mut b_recv,
                &bob,
                &binding,
                Duration::from_secs(4),
            )
            .await
        });
        for _ in 0..4 {
            tokio::task::yield_now().await;
        }
        for _ in 0..3 {
            tokio::time::advance(Duration::from_secs(4)).await;
            for _ in 0..4 {
                tokio::task::yield_now().await;
            }
        }
        assert!(initiator.await.unwrap().is_ok());
        assert!(responder.await.unwrap().is_ok());
    }

    #[tokio::test]
    async fn impersonated_node_id_is_rejected() {
        let alice = Identity::generate();
        let mallory = Identity::generate();
        let binding = test_binding();
        let ((mut m_recv, mut m_send), (mut b_recv, mut b_send)) = split_pair();
        let responder = tokio::spawn(async move {
            let bob = Identity::generate();
            handshake_responder(&mut b_send, &mut b_recv, &bob, &binding).await
        });
        write_frame(
            &mut m_send,
            &ControlMessage::Hello {
                protocol_version: PROTOCOL_VERSION,
                node_id: alice.node_id(),
                public_key: mallory.public_key_bytes(),
                nonce: [1u8; 32],
            },
        )
        .await
        .unwrap();
        assert!(responder.await.unwrap().is_err());
        let _ = read_frame(&mut m_recv).await;
    }

    #[tokio::test]
    async fn bad_signature_is_rejected() {
        let mallory = Identity::generate();
        let third_party = Identity::generate();
        let bob = Identity::generate();
        let bob_public_key = bob.public_key();
        let binding = test_binding();
        let ((mut m_recv, mut m_send), (mut b_recv, mut b_send)) = split_pair();
        let responder = tokio::spawn(async move {
            handshake_responder(&mut b_send, &mut b_recv, &bob, &binding).await
        });
        let mallory_nonce = [2u8; 32];
        write_frame(
            &mut m_send,
            &ControlMessage::Hello {
                protocol_version: PROTOCOL_VERSION,
                node_id: mallory.node_id(),
                public_key: mallory.public_key_bytes(),
                nonce: mallory_nonce,
            },
        )
        .await
        .unwrap();
        let ack = read_frame(&mut m_recv).await.unwrap().unwrap();
        let bob_nonce = match ack {
            ControlMessage::HelloAck {
                nonce, peer_nonce, ..
            } => {
                assert_eq!(peer_nonce, mallory_nonce);
                nonce
            }
            other => panic!("期待 HelloAck，收到 {}", other.kind()),
        };
        let payload = handshake_payload(
            &mallory.public_key_bytes(),
            &mallory_nonce,
            &bob_public_key.to_bytes(),
            &bob_nonce,
            binding.as_bytes(),
        );
        let bad_signature = third_party.sign(&payload);
        write_frame(
            &mut m_send,
            &ControlMessage::Auth {
                signature: bad_signature.to_bytes().to_vec(),
            },
        )
        .await
        .unwrap();
        assert!(responder.await.unwrap().is_err());
    }

    #[tokio::test]
    async fn version_mismatch_and_v2_are_rejected() {
        for version in [PROTOCOL_VERSION + 1, 2] {
            let mallory = Identity::generate();
            let binding = test_binding();
            let ((_m_recv, mut m_send), (mut b_recv, mut b_send)) = split_pair();
            let responder = tokio::spawn(async move {
                let bob = Identity::generate();
                handshake_responder(&mut b_send, &mut b_recv, &bob, &binding).await
            });
            write_frame(
                &mut m_send,
                &ControlMessage::Hello {
                    protocol_version: version,
                    node_id: mallory.node_id(),
                    public_key: mallory.public_key_bytes(),
                    nonce: [3u8; 32],
                },
            )
            .await
            .unwrap();
            let error = responder.await.unwrap().expect_err("版本必须被拒绝");
            assert!(error.to_string().contains("协议版本不兼容"));
        }
    }

    #[tokio::test]
    async fn ready_or_abort_before_hello_is_rejected() {
        for message in [
            ControlMessage::Ready,
            ControlMessage::Abort {
                reason: "unsupported".into(),
            },
        ] {
            let binding = test_binding();
            let ((_m_recv, mut m_send), (mut b_recv, mut b_send)) = split_pair();
            let responder = tokio::spawn(async move {
                let bob = Identity::generate();
                handshake_responder(&mut b_send, &mut b_recv, &bob, &binding).await
            });
            write_frame(&mut m_send, &message).await.unwrap();
            assert!(responder.await.unwrap().is_err());
        }
    }
}
