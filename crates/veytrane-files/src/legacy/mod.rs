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
