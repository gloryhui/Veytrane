//! Frozen node capability bits shared by negotiation and dispatch.

pub const CAP_TASK_PROTOCOL: u64 = 1;
pub const CAP_RELATIVE_ENTRIES: u64 = 2;
pub const CAP_PAUSE_RESUME: u64 = 4;
pub const CAP_SPEED_OWNERSHIP: u64 = 8;
pub const CAP_FILE_TRANSFER: u64 = 16;
pub const CAP_DIRECTORY_TRANSFER: u64 = 32;
pub const CAP_SPEED_EXECUTION: u64 = 64;
pub const CAP_TCP_TUNNEL: u64 = 128;
pub const CAP_REMOTE_AUTH: u64 = 256;
pub const CAP_TRUSTED_DEVICE_AUTH: u64 = 1 << 9;
pub const CAP_FILE_FLOW: u64 = 1 << 10;
pub const CAP_AUTO_RESUME: u64 = 1 << 11;
pub const CAP_INCREMENTAL_DIRECTORY: u64 = 1 << 12;

pub const CAP_NODE_BASE: u64 = 1 << 16;
pub const CAP_SERVICE_DIRECTORY: u64 = 1 << 17;
pub const CAP_RESOURCE_AUTHORIZATION: u64 = 1 << 18;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_bits_keep_the_frozen_values() {
        assert_eq!(
            [
                CAP_TASK_PROTOCOL,
                CAP_RELATIVE_ENTRIES,
                CAP_PAUSE_RESUME,
                CAP_SPEED_OWNERSHIP,
                CAP_FILE_TRANSFER,
                CAP_DIRECTORY_TRANSFER,
                CAP_SPEED_EXECUTION,
                CAP_TCP_TUNNEL,
                CAP_REMOTE_AUTH,
                CAP_TRUSTED_DEVICE_AUTH,
                CAP_FILE_FLOW,
                CAP_AUTO_RESUME,
                CAP_INCREMENTAL_DIRECTORY,
                CAP_NODE_BASE,
                CAP_SERVICE_DIRECTORY,
                CAP_RESOURCE_AUTHORIZATION,
            ],
            [
                1, 2, 4, 8, 16, 32, 64, 128, 256, 512, 1024, 2048, 4096, 65536, 131072, 262144
            ]
        );
    }
}
