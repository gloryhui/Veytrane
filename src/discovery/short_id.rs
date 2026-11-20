//! Public device addresses and optional signaling-side allocation.

#[cfg(any(feature = "signaling-server", test))]
pub use super::short_id_store::{ALLOCATION_WINDOW_SECONDS, AllocationLimits, ShortIdStore};
pub use veytrane_transport::short_id::{FIRST_SHORT_ID, LAST_SHORT_ID, ShortId};
