//! Speed lease control and coordinator arbitration.

use serde::{Deserialize, Serialize};

use veytrane_node::operation_id::TaskId;
use veytrane_transport::error::{Error, Result};
use veytrane_transport::identity::NodeId;

fn invalid(message: &str) -> Error {
    Error::Protocol(message.into())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum SpeedDirection {
    Upload,
    Download,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SpeedLease {
    pub test_id: TaskId,
    pub owner: NodeId,
    pub direction: SpeedDirection,
    pub seconds: u16,
    pub stream_token: [u8; 16],
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum SpeedControl {
    Request {
        test_id: TaskId,
        direction: SpeedDirection,
        seconds: u16,
    },
    Granted(SpeedLease),
    Busy {
        test_id: TaskId,
    },
    Cancel(SpeedLease),
    Finished(SpeedLease),
    Ready(SpeedLease),
    Result {
        lease: SpeedLease,
        bytes: u64,
        elapsed_ms: u64,
    },
}

impl SpeedControl {
    pub fn test_id(&self) -> &TaskId {
        match self {
            Self::Request { test_id, .. } | Self::Busy { test_id } => test_id,
            Self::Granted(lease)
            | Self::Cancel(lease)
            | Self::Finished(lease)
            | Self::Ready(lease)
            | Self::Result { lease, .. } => &lease.test_id,
        }
    }

    pub fn validate(&self) -> Result<()> {
        let seconds = match self {
            Self::Request { seconds, .. } => *seconds,
            Self::Granted(lease)
            | Self::Cancel(lease)
            | Self::Finished(lease)
            | Self::Ready(lease)
            | Self::Result { lease, .. } => lease.seconds,
            Self::Busy { .. } => return Ok(()),
        };
        validate_speed_seconds(seconds)
    }
}

fn validate_speed_seconds(seconds: u16) -> Result<()> {
    if seconds != 30 && (!(60..=600).contains(&seconds) || !seconds.is_multiple_of(60)) {
        return Err(invalid("测速时长非法"));
    }
    Ok(())
}

/// Only the lower NodeId grants leases. Both UIs route requests to that one owner.
#[derive(Debug)]
pub struct SpeedArbiter {
    local: NodeId,
    peer: NodeId,
    active: Option<SpeedLease>,
    stream_claimed: bool,
}

impl SpeedArbiter {
    pub fn new(local: NodeId, peer: NodeId) -> Self {
        Self {
            local,
            peer,
            active: None,
            stream_claimed: false,
        }
    }

    pub fn request(
        &mut self,
        requester: NodeId,
        test_id: TaskId,
        direction: SpeedDirection,
        seconds: u16,
        active_files: usize,
    ) -> Result<SpeedLease> {
        if self.local >= self.peer {
            return Err(invalid("测速请求必须交给较小 NodeId 的协调者"));
        }
        if requester != self.local && requester != self.peer {
            return Err(invalid("测速请求对端身份不符"));
        }
        validate_speed_seconds(seconds)?;
        if active_files != 0 || self.active.is_some() {
            return Err(invalid("测速或文件任务正在进行"));
        }
        let lease = SpeedLease {
            test_id,
            owner: requester,
            direction,
            seconds,
            stream_token: rand::random(),
        };
        self.active = Some(lease.clone());
        self.stream_claimed = false;
        Ok(lease)
    }

    pub fn accept_grant(
        &mut self,
        authenticated_grantor: NodeId,
        lease: SpeedLease,
        active_files: usize,
    ) -> Result<()> {
        if self.peer >= self.local
            || authenticated_grantor != self.peer
            || (lease.owner != self.local && lease.owner != self.peer)
            || self.active.is_some()
            || active_files != 0
        {
            return Err(invalid("测速授权来源、归属或状态非法"));
        }
        validate_speed_seconds(lease.seconds)?;
        self.active = Some(lease);
        self.stream_claimed = false;
        Ok(())
    }

    pub fn claim_stream(&mut self, authenticated_sender: NodeId, lease: &SpeedLease) -> Result<()> {
        let source = match lease.direction {
            SpeedDirection::Upload => lease.owner,
            SpeedDirection::Download if lease.owner == self.local => self.peer,
            SpeedDirection::Download => self.local,
        };
        if self.active.as_ref() != Some(lease)
            || self.stream_claimed
            || authenticated_sender != source
        {
            return Err(invalid("测速数据流归属或令牌不符"));
        }
        self.stream_claimed = true;
        Ok(())
    }

    pub fn finish(&mut self, authenticated_actor: NodeId, lease: &SpeedLease) -> Result<()> {
        if (authenticated_actor != self.local && authenticated_actor != self.peer)
            || self.active.as_ref() != Some(lease)
        {
            return Err(invalid("测速任务身份不符"));
        }
        self.active = None;
        self.stream_claimed = false;
        Ok(())
    }

    pub fn can_start_file(&self) -> bool {
        self.active.is_none()
    }
}
