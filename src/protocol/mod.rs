//! 应用层协议：控制消息、文件清单、帧编解码。

#[cfg(feature = "standalone")]
pub mod frame;
#[cfg(feature = "standalone-files")]
pub mod manifest;
#[cfg(feature = "standalone")]
pub mod message;
pub mod raw_frame;

#[cfg(feature = "standalone")]
pub use frame::{MAX_FRAME_LEN, read_frame, read_raw_frame, write_frame, write_raw_frame};
#[cfg(feature = "standalone-files")]
pub use manifest::{ChunkHash, DEFAULT_CHUNK_SIZE, FileManifest};
#[cfg(feature = "standalone")]
pub use message::ControlMessage;

#[cfg(feature = "standalone")]
pub use message::PROTOCOL_VERSION;
#[cfg(not(feature = "standalone"))]
pub use veytrane_transport::identity_wire::IDENTITY_PROTOCOL_VERSION as PROTOCOL_VERSION;
