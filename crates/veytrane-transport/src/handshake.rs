//! 应用层握手：把「对面是谁」这件事钉死。
//!
//! QUIC 已经把通道加密了，但加密不等于知道对面是谁——因为证书是自签的。
//! 握手的作用就是补上身份这一环：
//!
//! ```text
//! 发起方                                接收方
//!   Hello { id, pubkey, nonce_a }  ──────►
//!                                  ◄────── HelloAck { id, pubkey, nonce_b, sig_b }
//!   Auth { sig_a }                 ──────►
//!                                  ◄────── Ready
//! ```
//!
//! 签名载荷包含双方公钥、双方随机数，以及当前 QUIC/TLS 会话导出的 channel
//! binding。双方都签名，因此同时确认对端身份和这一次 TLS 会话。收到的公钥
//! 必须能推导出对端声称的节点 ID，否则视为冒名。
//!
//! 绑定值只能来自当前连接，见 [`ChannelBinding::from_connection`]。没有它，
//! 攻击者可以建立两条独立连接并原样转发握手消息。

use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite};

use crate::channel_binding::ChannelBinding;
use crate::error::{Error, Result};
use crate::identity::{
    Identity, NodeId, public_key_from_bytes, signature_from_bytes, verify_signature,
};
use crate::identity_wire::{
    IDENTITY_PROTOCOL_VERSION, IdentityMessage, handshake_payload, read_identity_frame,
    write_identity_frame,
};

/// 应用层握手各阶段允许的最大静默时间。
pub const APPLICATION_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Debug)]
pub struct HandshakeOutcome {
    pub peer_node_id: NodeId,
    pub peer_public_key: ed25519_dalek::VerifyingKey,
}

pub async fn prepare_handshake_initiator<S, R>(
    send: &mut S,
    recv: &mut R,
    identity: &Identity,
    binding: &ChannelBinding,
) -> Result<PreparedHandshake>
where
    S: AsyncWrite + Unpin,
    R: AsyncRead + Unpin,
{
    let nonce: [u8; 32] = rand::random();

    write_handshake_frame(
        send,
        &IdentityMessage::Hello {
            protocol_version: IDENTITY_PROTOCOL_VERSION,
            node_id: identity.node_id(),
            public_key: identity.public_key_bytes(),
            nonce,
        },
        "发送 Hello",
    )
    .await?;

    let reply = read_handshake_frame(recv, "等待 HelloAck").await?;

    let (peer_nonce, peer_public_key, peer_node_id, signature) = match reply {
        IdentityMessage::HelloAck {
            protocol_version,
            node_id,
            public_key,
            nonce: peer_nonce,
            peer_nonce: echoed,
            signature,
        } => {
            check_version(protocol_version)?;
            if echoed != nonce {
                return Err(Error::Protocol(
                    "HelloAck 回显的随机数与发出的不符，可能是重放的应答".into(),
                ));
            }
            let key = public_key_from_bytes(&public_key)?;
            check_node_id(&key, node_id)?;
            (peer_nonce, key, node_id, signature)
        }
        other => {
            return Err(Error::Protocol(format!(
                "握手阶段收到意外消息 {}",
                other.kind()
            )));
        }
    };

    let payload = handshake_payload(
        &identity.public_key_bytes(),
        &nonce,
        &peer_public_key.to_bytes(),
        &peer_nonce,
        binding.as_bytes(),
    );
    verify_signature(
        &peer_public_key,
        &payload,
        &signature_from_bytes(&signature)?,
    )?;

    Ok(PreparedHandshake {
        outcome: HandshakeOutcome {
            peer_node_id,
            peer_public_key,
        },
        signature: identity.sign(&payload).to_bytes().to_vec(),
    })
}

pub struct PreparedHandshake {
    pub outcome: HandshakeOutcome,
    signature: Vec<u8>,
}

impl PreparedHandshake {
    pub async fn finish<S: AsyncWrite + Unpin, R: AsyncRead + Unpin>(
        self,
        send: &mut S,
        recv: &mut R,
    ) -> Result<HandshakeOutcome> {
        write_handshake_frame(
            send,
            &IdentityMessage::Auth {
                signature: self.signature,
            },
            "发送 Auth",
        )
        .await?;
        expect_ready(recv).await?;
        Ok(self.outcome)
    }
}

pub async fn handshake_initiator<S: AsyncWrite + Unpin, R: AsyncRead + Unpin>(
    send: &mut S,
    recv: &mut R,
    identity: &Identity,
    binding: &ChannelBinding,
) -> Result<HandshakeOutcome> {
    prepare_handshake_initiator(send, recv, identity, binding)
        .await?
        .finish(send, recv)
        .await
}

pub async fn prepare_handshake_responder<S, R>(
    send: &mut S,
    recv: &mut R,
    identity: &Identity,
    binding: &ChannelBinding,
) -> Result<VerifiedResponder>
where
    S: AsyncWrite + Unpin,
    R: AsyncRead + Unpin,
{
    let hello = read_handshake_frame(recv, "等待 Hello").await?;

    let (peer_nonce, peer_public_key, peer_node_id) = match hello {
        IdentityMessage::Hello {
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
                "握手阶段期待 Hello，收到 {}",
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

    let our_signature = identity.sign(&payload);
    write_handshake_frame(
        send,
        &IdentityMessage::HelloAck {
            protocol_version: IDENTITY_PROTOCOL_VERSION,
            node_id: identity.node_id(),
            public_key: identity.public_key_bytes(),
            nonce: our_nonce,
            peer_nonce,
            signature: our_signature.to_bytes().to_vec(),
        },
        "发送 HelloAck",
    )
    .await?;

    let auth = read_handshake_frame(recv, "等待 Auth").await?;

    match auth {
        IdentityMessage::Auth { signature } => {
            verify_signature(
                &peer_public_key,
                &payload,
                &signature_from_bytes(&signature)?,
            )?;
        }
        other => {
            return Err(Error::Protocol(format!(
                "握手阶段期待 Auth，收到 {}",
                other.kind()
            )));
        }
    }

    Ok(VerifiedResponder {
        outcome: HandshakeOutcome {
            peer_node_id,
            peer_public_key,
        },
    })
}

pub struct VerifiedResponder {
    outcome: HandshakeOutcome,
}

impl VerifiedResponder {
    pub fn outcome(&self) -> &HandshakeOutcome {
        &self.outcome
    }

    pub async fn finish<S: AsyncWrite + Unpin>(self, send: &mut S) -> Result<HandshakeOutcome> {
        write_handshake_frame(send, &IdentityMessage::Ready, "发送 Ready").await?;
        Ok(self.outcome)
    }
}

pub async fn handshake_responder<S: AsyncWrite + Unpin, R: AsyncRead + Unpin>(
    send: &mut S,
    recv: &mut R,
    identity: &Identity,
    binding: &ChannelBinding,
) -> Result<HandshakeOutcome> {
    prepare_handshake_responder(send, recv, identity, binding)
        .await?
        .finish(send)
        .await
}

fn check_version(version: u32) -> Result<()> {
    if version != IDENTITY_PROTOCOL_VERSION {
        return Err(Error::Protocol(format!(
            "协议版本不兼容：本机 {IDENTITY_PROTOCOL_VERSION}，对端 {version}"
        )));
    }
    Ok(())
}

fn check_node_id(public_key: &ed25519_dalek::VerifyingKey, claimed: NodeId) -> Result<()> {
    let derived = NodeId::from_public_key(public_key);
    if derived != claimed {
        return Err(Error::Protocol(format!(
            "节点 ID 与公钥不符：声称 {}，公钥实为 {}",
            claimed.short(),
            derived.short()
        )));
    }
    Ok(())
}

async fn expect_ready<R>(recv: &mut R) -> Result<()>
where
    R: AsyncRead + Unpin,
{
    let message = read_handshake_frame(recv, "等待 Ready").await?;

    match message {
        IdentityMessage::Ready => Ok(()),
        other => Err(Error::Protocol(format!(
            "期待 Ready，收到 {}",
            other.kind()
        ))),
    }
}

async fn read_handshake_frame<R>(recv: &mut R, phase: &str) -> Result<IdentityMessage>
where
    R: AsyncRead + Unpin,
{
    tokio::time::timeout(APPLICATION_HANDSHAKE_TIMEOUT, read_identity_frame(recv))
        .await
        .map_err(|_| Error::Protocol(format!("{phase}超时")))??
        .ok_or_else(|| Error::Protocol(format!("{phase}时连接被关闭")))
}

async fn write_handshake_frame<S>(
    send: &mut S,
    message: &IdentityMessage,
    phase: &str,
) -> Result<()>
where
    S: AsyncWrite + Unpin,
{
    tokio::time::timeout(
        APPLICATION_HANDSHAKE_TIMEOUT,
        write_identity_frame(send, message),
    )
    .await
    .map_err(|_| Error::Protocol(format!("{phase}超时")))??;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[tokio::test]
    async fn handshake_authenticates_both_directions() {
        let alice = Identity::generate();
        let bob = Identity::generate();
        let binding = test_binding();
        let ((mut a_recv, mut a_send), (mut b_recv, mut b_send)) = split_pair();

        let (alice_result, bob_result) = tokio::join!(
            handshake_initiator(&mut a_send, &mut a_recv, &alice, &binding),
            handshake_responder(&mut b_send, &mut b_recv, &bob, &binding),
        );

        let alice_outcome = alice_result.unwrap();
        let bob_outcome = bob_result.unwrap();
        assert_eq!(alice_outcome.peer_node_id, bob.node_id());
        assert_eq!(bob_outcome.peer_node_id, alice.node_id());
        assert_eq!(alice_outcome.peer_public_key, bob.public_key());
        assert_eq!(bob_outcome.peer_public_key, alice.public_key());
    }

    #[tokio::test(start_paused = true)]
    async fn handshake_stage_timeout_fails() {
        let alice = Identity::generate();
        let binding = test_binding();
        let ((mut a_recv, mut a_send), (mut b_recv, mut b_send)) = split_pair();
        let silent_peer = tokio::spawn(async move {
            let _keep_alive = (&mut b_recv, &mut b_send);
            std::future::pending::<()>().await;
        });
        let initiator = tokio::spawn(async move {
            handshake_initiator(&mut a_send, &mut a_recv, &alice, &binding).await
        });

        for _ in 0..4 {
            tokio::task::yield_now().await;
        }
        tokio::time::advance(APPLICATION_HANDSHAKE_TIMEOUT + Duration::from_secs(1)).await;
        let error = initiator.await.unwrap().expect_err("静默阶段必须超时");
        assert!(error.to_string().contains("超时"));
        silent_peer.abort();
    }

    #[tokio::test]
    async fn closed_peer_fails_without_hanging() {
        let binding = test_binding();
        let ((m_recv, m_send), (mut b_recv, mut b_send)) = split_pair();
        drop(m_recv);
        drop(m_send);

        let bob = Identity::generate();
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            handshake_responder(&mut b_send, &mut b_recv, &bob, &binding),
        )
        .await
        .expect("不该挂死");
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn wrong_channel_binding_fails() {
        let alice = Identity::generate();
        let bob = Identity::generate();
        let mut flipped = *test_binding().as_bytes();
        flipped[13] ^= 0x01;
        let alice_binding = test_binding();
        let bob_binding = ChannelBinding::from_bytes_for_test(flipped);
        let ((mut a_recv, mut a_send), (mut b_recv, mut b_send)) = split_pair();

        let initiator = tokio::spawn(async move {
            handshake_initiator(&mut a_send, &mut a_recv, &alice, &alice_binding).await
        });
        let responder = tokio::spawn(async move {
            handshake_responder(&mut b_send, &mut b_recv, &bob, &bob_binding).await
        });
        let (alice_result, bob_result) = tokio::time::timeout(Duration::from_secs(5), async {
            (initiator.await.unwrap(), responder.await.unwrap())
        })
        .await
        .expect("绑定值不同时不该挂死");
        assert!(matches!(alice_result, Err(Error::Identity(_))));
        assert!(bob_result.is_err());
    }

    #[tokio::test]
    async fn relayed_same_session_succeeds_and_cross_session_fails() {
        run_relay_handshake([0x33; 32], [0x33; 32], true).await;
        run_relay_handshake([0x11; 32], [0x22; 32], false).await;
    }

    async fn run_relay_handshake(
        initiator_binding: [u8; 32],
        responder_binding: [u8; 32],
        expect_success: bool,
    ) {
        tokio::time::timeout(
            Duration::from_secs(10),
            relay_handshake(initiator_binding, responder_binding, expect_success),
        )
        .await
        .expect("转发握手不该挂死");
    }

    async fn relay_handshake(
        initiator_binding: [u8; 32],
        responder_binding: [u8; 32],
        expect_success: bool,
    ) {
        use tokio::io::AsyncWriteExt;

        let alice = Identity::generate();
        let bob = Identity::generate();
        let ((mut a_recv, mut a_send), (m_a_recv, m_a_send)) = split_pair();
        let ((m_b_recv, m_b_send), (mut b_recv, mut b_send)) = split_pair();
        let initiator_binding = ChannelBinding::from_bytes_for_test(initiator_binding);
        let responder_binding = ChannelBinding::from_bytes_for_test(responder_binding);

        let initiator = tokio::spawn(async move {
            handshake_initiator(&mut a_send, &mut a_recv, &alice, &initiator_binding).await
        });
        let responder = tokio::spawn(async move {
            handshake_responder(&mut b_send, &mut b_recv, &bob, &responder_binding).await
        });
        let relay = tokio::spawn(async move {
            let a_to_b = tokio::spawn(async move {
                let mut from = m_a_recv;
                let mut to = m_b_send;
                let copied = tokio::io::copy(&mut from, &mut to).await;
                let _ = to.shutdown().await;
                copied
            });
            let b_to_a = tokio::spawn(async move {
                let mut from = m_b_recv;
                let mut to = m_a_send;
                let copied = tokio::io::copy(&mut from, &mut to).await;
                let _ = to.shutdown().await;
                copied
            });
            let _ = a_to_b.await;
            let _ = b_to_a.await;
        });

        let initiator_result = initiator.await.unwrap();
        let responder_result = responder.await.unwrap();
        relay.await.unwrap();
        if expect_success {
            assert!(initiator_result.is_ok(), "{initiator_result:?}");
            assert!(responder_result.is_ok(), "{responder_result:?}");
        } else {
            assert!(matches!(initiator_result, Err(Error::Identity(_))));
            assert!(responder_result.is_err());
        }
    }
}
