use crate::{
    error::Result,
    manifest::{ChunkHash, FileManifest},
};
use tokio::io::{AsyncRead, AsyncWrite};
use veytrane_transport::legacy_control::{self, ManifestPayload};

pub mod receiver;
pub mod sender;

pub type ControlMessage = legacy_control::ControlMessage<FileManifest, ChunkHash>;

impl ManifestPayload for FileManifest {
    fn validate(&self) -> Result<()> {
        FileManifest::validate(self)
    }
}

pub async fn read_frame(reader: &mut (impl AsyncRead + Unpin)) -> Result<Option<ControlMessage>> {
    legacy_control::read_frame(reader).await
}

pub async fn write_frame(
    writer: &mut (impl AsyncWrite + Unpin),
    message: &ControlMessage,
) -> Result<()> {
    legacy_control::write_frame(writer, message).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_base_decodes_tunnel_but_rejects_file_manifest() {
        type Base = legacy_control::ControlMessage<legacy_control::DisabledManifest, [u8; 32]>;

        let target = "127.0.0.1:12345";
        let tunnel = ControlMessage::TunnelOpen {
            target: target.into(),
        };
        assert_eq!(
            Base::decode(&tunnel.encode().unwrap()).unwrap(),
            Base::TunnelOpen {
                target: target.into(),
            },
        );
        let manifest =
            FileManifest::new("fixture.txt", 0, crate::manifest::MIN_CHUNK_SIZE, vec![]).unwrap();
        let message = ControlMessage::Manifest(Box::new(manifest));
        assert!(Base::decode(&message.encode().unwrap()).is_err());
    }
}
