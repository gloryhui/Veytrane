//! Compatibility path for the legacy standalone control messages.

#[cfg(feature = "standalone-files")]
pub use veytrane_files::legacy::ControlMessage;
#[cfg(not(feature = "standalone-files"))]
pub type ControlMessage = veytrane_transport::legacy_control::ControlMessage<
    veytrane_transport::legacy_control::DisabledManifest,
    [u8; 32],
>;
pub use veytrane_transport::legacy_control::{PROTOCOL_VERSION, SpeedTestWireDirection};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::{Identity, NodeId};
    #[cfg(feature = "standalone-files")]
    use crate::protocol::manifest::FileManifest;

    #[test]
    fn 消息编解码往返() {
        let identity = Identity::generate();
        let messages = vec![
            ControlMessage::Hello {
                protocol_version: PROTOCOL_VERSION,
                node_id: identity.node_id(),
                public_key: identity.public_key_bytes(),
                nonce: [1u8; 32],
            },
            ControlMessage::Auth {
                signature: vec![9u8; 64],
            },
            ControlMessage::Ready,
            ControlMessage::Resume {
                have: vec![0b1010_1010, 0b0000_0001],
            },
            ControlMessage::RequestChunk { index: 42 },
            ControlMessage::Chunk {
                index: 42,
                data: vec![0xab; 1024],
            },
            ControlMessage::TunnelOpen {
                target: "127.0.0.1:22".into(),
            },
            ControlMessage::TunnelReady,
            ControlMessage::TunnelError {
                reason: "目标拒绝连接".into(),
            },
            ControlMessage::KeepAlive,
            ControlMessage::Abort {
                reason: "对端掉线".into(),
            },
            ControlMessage::Bye,
            ControlMessage::SpeedTestOpen {
                direction: SpeedTestWireDirection::Upload,
                duration_ms: 10_000,
                block_size: 1024 * 1024,
            },
            ControlMessage::SpeedTestReady,
            ControlMessage::SpeedTestResult {
                bytes: 123,
                elapsed_ms: 456,
            },
        ];

        for message in messages {
            let bytes = message.encode().unwrap();
            let decoded = ControlMessage::decode(&bytes).unwrap();
            assert_eq!(message, decoded, "{} 往返失败", message.kind());
        }
    }

    #[test]
    fn v3_identity_frames_match_fixed_bytes() {
        let node_id = NodeId::from_hex("11111111111111111111111111111111").unwrap();
        let hello = ControlMessage::Hello {
            protocol_version: 3,
            node_id,
            public_key: [0x22; 32],
            nonce: [0x33; 32],
        };
        let mut hello_bytes = vec![0, 3];
        hello_bytes.extend_from_slice(&[0x11; 16]);
        hello_bytes.extend_from_slice(&[0x22; 32]);
        hello_bytes.extend_from_slice(&[0x33; 32]);

        let ack = ControlMessage::HelloAck {
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

        let auth = ControlMessage::Auth {
            signature: vec![0x55; 64],
        };
        let mut auth_bytes = vec![2, 64];
        auth_bytes.extend_from_slice(&[0x55; 64]);

        for (message, bytes) in [
            (hello, hello_bytes),
            (ack, ack_bytes),
            (auth, auth_bytes),
            (ControlMessage::Ready, vec![3]),
        ] {
            assert_eq!(message.encode().unwrap(), bytes);
            assert_eq!(ControlMessage::decode(&bytes).unwrap(), message);
        }
    }

    #[test]
    fn 追加测速消息不改变已有消息编号() {
        // postcard 对 enum 使用声明顺序编码。测速消息追加在末尾，保留 v3
        // 已有控制消息的 discriminant，因此不需要升级协议版本。
        assert_eq!(ControlMessage::Ready.encode().unwrap()[0], 3);
        assert_eq!(ControlMessage::Bye.encode().unwrap()[0], 15);
        assert_eq!(
            ControlMessage::SpeedTestOpen {
                direction: SpeedTestWireDirection::Upload,
                duration_ms: 1_000,
                block_size: 16 * 1024,
            }
            .encode()
            .unwrap()[0],
            16
        );
    }

    #[test]
    #[cfg(feature = "standalone-files")]
    fn 清单消息往返() {
        let manifest = FileManifest::new("a.bin", 0, 65536, vec![]).unwrap();
        let message = ControlMessage::Manifest(Box::new(manifest));
        let decoded = ControlMessage::decode(&message.encode().unwrap()).unwrap();
        assert_eq!(message, decoded);
    }

    /// Issue #3：畸形清单必须在解码这一层就被拒绝。
    ///
    /// 旧代码 `decode` 只是 `postcard::from_bytes`，攻击者可以自己算出根哈希，
    /// 却把 `chunk_size` 写成 0 —— 解出来的清单一路走到接收端才 `div_ceil(0)` panic。
    #[test]
    #[cfg(feature = "standalone-files")]
    fn 畸形清单在解码时就被拒绝() {
        use crate::error::Error;
        use crate::protocol::manifest::{ChunkHash, MIN_CHUNK_SIZE, root_hash_of};

        // chunk_size = 0，但根哈希是自洽的（攻击者自己算的）。
        let zero_chunk_size = {
            let chunks = vec![ChunkHash::of(b"x")];
            let root_hash = root_hash_of("evil.bin", 1024, 0, &chunks);
            FileManifest {
                file_name: "evil.bin".into(),
                total_len: 1024,
                chunk_size: 0,
                chunks,
                root_hash,
            }
        };
        let bytes = ControlMessage::Manifest(Box::new(zero_chunk_size.clone()))
            .encode()
            .unwrap();
        // postcard 自己能解出来，说明「能解码」不代表「可用」。
        assert!(postcard::from_bytes::<ControlMessage>(&bytes).is_ok());
        let err = ControlMessage::decode(&bytes).unwrap_err();
        assert!(matches!(err, Error::Protocol(_)), "实际 {err:?}");

        // 分片数与 total_len 不符：根哈希同样自洽。
        let mismatched = {
            let chunks = vec![ChunkHash::of(b"a")];
            let root_hash = root_hash_of("a.bin", 10_000_000, MIN_CHUNK_SIZE, &chunks);
            FileManifest {
                file_name: "a.bin".into(),
                total_len: 10_000_000,
                chunk_size: MIN_CHUNK_SIZE,
                chunks,
                root_hash,
            }
        };
        let bytes = ControlMessage::Manifest(Box::new(mismatched))
            .encode()
            .unwrap();
        assert!(matches!(
            ControlMessage::decode(&bytes).unwrap_err(),
            Error::Protocol(_)
        ));

        // 根哈希根本不对。
        let mut bad_root = FileManifest::new("b.bin", 0, MIN_CHUNK_SIZE, vec![]).unwrap();
        bad_root.root_hash = ChunkHash::of(b"nope");
        let bytes = ControlMessage::Manifest(Box::new(bad_root))
            .encode()
            .unwrap();
        assert!(matches!(
            ControlMessage::decode(&bytes).unwrap_err(),
            Error::Protocol(_)
        ));
    }

    #[test]
    fn 节点_id_能编码进消息() {
        let id = NodeId::from_hex("00112233445566778899aabbccddeeff").unwrap();
        let message = ControlMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
            node_id: id,
            public_key: [0u8; 32],
            nonce: [0u8; 32],
        };
        let decoded = ControlMessage::decode(&message.encode().unwrap()).unwrap();
        match decoded {
            ControlMessage::Hello { node_id, .. } => assert_eq!(node_id, id),
            other => panic!("意料之外的消息: {other:?}"),
        }
    }
}
