//! Compatibility path for the file protocol and shared capability/speed IDs.

pub use veytrane_files::protocol::{
    Actor, Difference, Entry, ErrorCode, Frame, MAX_TASKS_PER_PEER, Message, TaskProtocol,
    WireTaskState, read, validate_relative_path, write,
};
pub use veytrane_files::task_model::MAX_CHUNKS;
pub use veytrane_node::capability::{
    CAP_AUTO_RESUME, CAP_DIRECTORY_TRANSFER, CAP_FILE_FLOW, CAP_FILE_TRANSFER,
    CAP_INCREMENTAL_DIRECTORY, CAP_PAUSE_RESUME, CAP_RELATIVE_ENTRIES, CAP_REMOTE_AUTH,
    CAP_SPEED_EXECUTION, CAP_SPEED_OWNERSHIP, CAP_TASK_PROTOCOL, CAP_TCP_TUNNEL,
    CAP_TRUSTED_DEVICE_AUTH,
};
pub use veytrane_node::p2pf::MAX_FRAME_BYTES;
pub use veytrane_speed::control::{SpeedArbiter, SpeedControl, SpeedDirection, SpeedLease};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::Identity;
    use crate::node::core::task_model::TaskId;

    fn task() -> TaskId {
        TaskId::parse("00112233445566778899aabbccddeeff").unwrap()
    }

    #[test]
    fn only_one_speed_coordinator_can_grant_and_stream_claim_is_bound() {
        let a = Identity::generate().node_id();
        let b = Identity::generate().node_id();
        let (lower, higher) = if a < b { (a, b) } else { (b, a) };
        let mut coordinator = SpeedArbiter::new(lower, higher);
        let mut follower = SpeedArbiter::new(higher, lower);
        assert!(
            follower
                .request(higher, task(), SpeedDirection::Upload, 30, 0)
                .is_err()
        );
        assert!(
            coordinator
                .request(higher, task(), SpeedDirection::Upload, 30, 1)
                .is_err()
        );
        assert!(
            coordinator
                .request(higher, task(), SpeedDirection::Upload, 31, 0)
                .is_err()
        );
        let lease = coordinator
            .request(higher, task(), SpeedDirection::Upload, 600, 0)
            .unwrap();
        assert!(!coordinator.can_start_file());
        assert!(
            coordinator
                .request(lower, TaskId::generate(), SpeedDirection::Download, 30, 0)
                .is_err()
        );
        let mut stale = lease.clone();
        stale.stream_token[0] ^= 1;
        assert!(coordinator.claim_stream(higher, &stale).is_err());
        assert!(coordinator.claim_stream(lower, &lease).is_err());
        coordinator.claim_stream(higher, &lease).unwrap();
        assert!(coordinator.claim_stream(higher, &lease).is_err());
        assert!(coordinator.finish(higher, &stale).is_err());
        assert!(
            coordinator
                .finish(Identity::generate().node_id(), &lease)
                .is_err()
        );
        follower.accept_grant(lower, lease.clone(), 0).unwrap();
        assert!(!follower.can_start_file());
        assert!(follower.accept_grant(lower, lease.clone(), 0).is_err());
        coordinator.finish(higher, &lease).unwrap();
        follower.finish(lower, &lease).unwrap();
        let new_lease = coordinator
            .request(higher, task(), SpeedDirection::Upload, 30, 0)
            .unwrap();
        assert!(coordinator.finish(higher, &lease).is_err());
        coordinator.finish(higher, &new_lease).unwrap();
        assert!(coordinator.can_start_file());
    }
}
