//! Root compatibility path for the shared raw frame codec.

pub use veytrane_transport::raw_frame::{
    MAX_FRAME_LEN, read_raw_frame, read_raw_frame_limited, write_raw_frame, write_raw_frame_limited,
};
