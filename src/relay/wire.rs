//! Root compatibility path for the shared Relay control wire.

pub use veytrane_transport::quic::relay::wire::{
    Challenge, Hello, MAGIC, Packet, bind_payload, decode, encode,
};
#[cfg(test)]
pub use veytrane_transport::quic::relay::wire::{MAX_CONTROL, register};
