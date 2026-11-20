//! 控制消息与握手签名载荷。

use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::identity::NodeId;
use crate::protocol::manifest::{ChunkHash, FileManifest};

/// 协议版本。不兼容改动时递增。
///
/// v3：文件传输的 `Complete` 只在接收端 finalize 成功后发送；同时保留 v2 的
/// TLS 会话绑定握手。v2 节点仍使用 finalize 前的 Complete 语义，必须在握手阶段
/// 以明确的版本错误拒绝，不能静默互通。
pub const PROTOCOL_VERSION: u32 = 3;

/// 握手/传输通道上流动的消息。
///
/// 注意：`Chunk` 直接携带整片数据，只适合初稿。真正跑大文件时数据应当走
/// 独立的单向流，避免控制消息和大块数据在同一个流里互相阻塞（TODO）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ControlMessage {
    // These four keep their v3 discriminants for the standalone CLI. The QUIC
    // identity handshake uses transport::identity_wire::IdentityMessage.
    /// 发起方打招呼，带上自己的公钥和随机数。
    Hello {
        protocol_version: u32,
        node_id: NodeId,
        public_key: [u8; 32],
        nonce: [u8; 32],
    },
    /// 接收方回应，并对双方公钥 + 双方随机数签名。
    HelloAck {
        protocol_version: u32,
        node_id: NodeId,
        public_key: [u8; 32],
        nonce: [u8; 32],
        peer_nonce: [u8; 32],
        signature: Vec<u8>,
    },
    /// 发起方的回签，让接收方也确认对面身份（双向认证）。
    Auth { signature: Vec<u8> },
    /// 双方确认身份，可以开始传。
    Ready,

    /// 发送端下发文件清单。
    Manifest(Box<FileManifest>),
    /// 接收端声明自己已校验通过的分片位图，用于断点续传。
    Resume { have: Vec<u8> },
    /// 接收端请求某一片。
    RequestChunk { index: u32 },
    /// 发送端回应一片数据。
    Chunk { index: u32, data: Vec<u8> },
    /// 接收端确认某一片已落盘并通过校验。
    ChunkAck { index: u32 },
    /// 全部收完，回报根哈希。
    Complete { root_hash: ChunkHash },

    /// 请求转发一条 TCP 连接到 `target`（形如 `127.0.0.1:22`）。
    TunnelOpen { target: String },
    /// 隧道已就绪，可以开始搬字节了。
    TunnelReady,
    /// 隧道建立失败。
    TunnelError { reason: String },

    /// 保活，维持 NAT 映射和 QUIC 连接。
    KeepAlive,
    /// 出错了，附带原因。
    Abort { reason: String },
    /// 礼貌结束。
    Bye,

    /// 请求一次内存到内存的 QUIC 测速。
    ///
    /// 测速方向只在控制面协商；实际 payload 走同一连接上的独立单向流，
    /// 不把每个 block 包成控制消息。
    SpeedTestOpen {
        direction: SpeedTestWireDirection,
        duration_ms: u64,
        block_size: u32,
    },
    /// 对端已经接受测速参数，可以打开数据流。
    SpeedTestReady,
    /// 测速数据源在数据流结束后报告本阶段统计。
    SpeedTestResult { bytes: u64, elapsed_ms: u64 },
}

/// 测速协议线上只允许单向的一轮；CLI 的 `both` 会顺序发起 upload 和 download。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SpeedTestWireDirection {
    Upload,
    Download,
}

impl ControlMessage {
    pub fn encode(&self) -> Result<Vec<u8>> {
        Ok(postcard::to_allocvec(self)?)
    }

    /// 解码控制消息。
    ///
    /// `Manifest` 是「不可信字节直接变成一个会被大量使用的结构体」的唯一入口，
    /// 所以在这里就把清单完整校验掉：解码成功的 `ControlMessage::Manifest`
    /// **一定**已经通过 [`FileManifest::validate`]，下游不必再怀疑它。这样
    /// 未来新增的接收路径也不会因为忘记校验而踩到畸形清单。
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let message: Self = postcard::from_bytes(bytes)?;
        if let Self::Manifest(manifest) = &message {
            manifest.validate()?;
        }
        Ok(message)
    }

    /// 便于日志阅读的简短名字。
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Hello { .. } => "Hello",
            Self::HelloAck { .. } => "HelloAck",
            Self::Auth { .. } => "Auth",
            Self::Ready => "Ready",
            Self::Manifest(_) => "Manifest",
            Self::Resume { .. } => "Resume",
            Self::RequestChunk { .. } => "RequestChunk",
            Self::Chunk { .. } => "Chunk",
            Self::ChunkAck { .. } => "ChunkAck",
            Self::Complete { .. } => "Complete",
            Self::TunnelOpen { .. } => "TunnelOpen",
            Self::TunnelReady => "TunnelReady",
            Self::TunnelError { .. } => "TunnelError",
            Self::KeepAlive => "KeepAlive",
            Self::Abort { .. } => "Abort",
            Self::Bye => "Bye",
            Self::SpeedTestOpen { .. } => "SpeedTestOpen",
            Self::SpeedTestReady => "SpeedTestReady",
            Self::SpeedTestResult { .. } => "SpeedTestResult",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::{Identity, NodeId};

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
