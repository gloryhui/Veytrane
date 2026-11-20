//! Compatibility path for the unified node BI dispatcher.

pub use veytrane_node::dispatch::{
    BiStream, Dispatcher, Incoming, MAX_PENDING_HEADERS, QUEUE_CAPACITY, RESET_BUSY, RESET_INVALID,
    RESET_UNAUTHORIZED, RESET_UNSUPPORTED, open_bi,
};
