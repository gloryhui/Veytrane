//! Legacy standalone control protocol with parameterized file payloads.

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use tokio::io::{AsyncRead, AsyncWrite};

use crate::error::{Error, Result};
use crate::identity::NodeId;
pub use crate::raw_frame::{
    MAX_FRAME_LEN, read_raw_frame, read_raw_frame_limited, write_raw_frame, write_raw_frame_limited,
};

pub const PROTOCOL_VERSION: u32 = 3;

pub trait ManifestPayload {
    fn validate(&self) -> Result<()>;
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ControlMessage<Manifest, Hash> {
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

    Manifest(Box<Manifest>),
    Resume {
        have: Vec<u8>,
    },
    RequestChunk {
        index: u32,
    },
    Chunk {
        index: u32,
        data: Vec<u8>,
    },
    ChunkAck {
        index: u32,
    },
    Complete {
        root_hash: Hash,
    },

    TunnelOpen {
        target: String,
    },
    TunnelReady,
    TunnelError {
        reason: String,
    },

    KeepAlive,
    Abort {
        reason: String,
    },
    Bye,

    SpeedTestOpen {
        direction: SpeedTestWireDirection,
        duration_ms: u64,
        block_size: u32,
    },
    SpeedTestReady,
    SpeedTestResult {
        bytes: u64,
        elapsed_ms: u64,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SpeedTestWireDirection {
    Upload,
    Download,
}

impl<Manifest, Hash> ControlMessage<Manifest, Hash> {
    pub fn encode(&self) -> Result<Vec<u8>>
    where
        Self: Serialize,
    {
        Ok(postcard::to_allocvec(self)?)
    }

    pub fn decode(bytes: &[u8]) -> Result<Self>
    where
        Self: DeserializeOwned,
        Manifest: ManifestPayload,
    {
        let message: Self = postcard::from_bytes(bytes)?;
        if let Self::Manifest(manifest) = &message {
            manifest.validate()?;
        }
        Ok(message)
    }

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

pub async fn write_frame<Manifest, Hash, W>(
    writer: &mut W,
    message: &ControlMessage<Manifest, Hash>,
) -> Result<()>
where
    W: AsyncWrite + Unpin,
    Manifest: Serialize,
    Hash: Serialize,
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

pub async fn read_frame<Manifest, Hash, R>(
    reader: &mut R,
) -> Result<Option<ControlMessage<Manifest, Hash>>>
where
    R: AsyncRead + Unpin,
    Manifest: DeserializeOwned + ManifestPayload,
    Hash: DeserializeOwned,
{
    match read_raw_frame(reader).await? {
        Some(payload) => Ok(Some(ControlMessage::decode(&payload)?)),
        None => Ok(None),
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum DisabledManifest {}

impl ManifestPayload for DisabledManifest {
    fn validate(&self) -> Result<()> {
        match *self {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_file_variants_keep_tunnel_and_speed_discriminants() {
        type NoFile = ControlMessage<DisabledManifest, [u8; 32]>;
        assert_eq!(PROTOCOL_VERSION, 3);
        assert_eq!(
            NoFile::TunnelOpen {
                target: "127.0.0.1:22".into(),
            }
            .encode()
            .unwrap()[0],
            10
        );
        assert_eq!(
            NoFile::SpeedTestOpen {
                direction: SpeedTestWireDirection::Upload,
                duration_ms: 1_000,
                block_size: 16 * 1024,
            }
            .encode()
            .unwrap()[0],
            16
        );
    }
}
