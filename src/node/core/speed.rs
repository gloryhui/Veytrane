//! Compatibility path for the speed runtime.

pub use veytrane_speed::runtime::{DEFAULT_MAX_PEER_BUSINESS_STREAMS, SpeedService, SpeedSnapshot};
#[cfg(test)]
pub use veytrane_speed::runtime::SpeedStatus;
