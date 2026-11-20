//! Framing constants shared by the P2PF file and speed codecs.

/// Identifies a P2PF payload.
pub const MAGIC: &[u8; 5] = b"P2PF\x01";
/// Upper bound for a complete payload, including its magic.
pub const MAX_FRAME_BYTES: u32 = 4 * 1024 * 1024;
