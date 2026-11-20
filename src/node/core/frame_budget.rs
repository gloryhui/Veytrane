//! File-frame decoding over the shared node frame budget.

use crate::error::Result;
use crate::node::core::protocol::Frame;
use std::sync::Arc;
use tokio::{io::AsyncRead, sync::Semaphore};

pub use veytrane_node::frame_budget::{BufferedBytes, FrameBudget, FrameLease};

pub struct BufferedFrame {
    pub frame: Frame,
    pub _lease: FrameLease,
}

pub(crate) async fn read_file_frame<R: AsyncRead + Unpin>(
    budget: &FrameBudget,
    reader: &mut R,
    task: Option<Arc<Semaphore>>,
) -> Result<BufferedFrame> {
    let BufferedBytes { bytes, _lease } = budget.read_raw(reader, task).await?;
    Ok(BufferedFrame {
        frame: Frame::decode(&bytes)?,
        _lease,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node::core::protocol::{self, Message};
    use crate::node::core::speed_wire::SpeedFrame;
    use crate::node::core::task_model::TaskId;
    use veytrane_node::frame_budget::{TASK_BYTES, UNIT};

    #[tokio::test]
    async fn decoded_payload_keeps_budget_until_processing_and_bad_body_releases_it() {
        let budget = FrameBudget::default();
        let task = FrameBudget::task_limit();
        let frame = Frame {
            request_id: 1,
            message: Message::Chunk {
                task_id: TaskId::generate(),
                index: 0,
                data: vec![7; 1024 * 1024],
            },
        };
        let encoded = frame.encode().unwrap();
        let units = (encoded.len() * 2).div_ceil(UNIT);
        let mut wire = (encoded.len() as u32).to_le_bytes().to_vec();
        wire.extend_from_slice(&encoded);
        let decoded = read_file_frame(&budget, &mut &wire[..], Some(task.clone()))
            .await
            .unwrap();
        assert_eq!(decoded.frame, frame);
        assert_eq!(task.available_permits(), TASK_BYTES / UNIT - units);
        drop(decoded);
        assert_eq!(task.available_permits(), TASK_BYTES / UNIT);
        let bad = [1, 0, 0, 0, 255];
        assert!(
            read_file_frame(&budget, &mut &bad[..], Some(task.clone()))
                .await
                .is_err()
        );
        assert_eq!(task.available_permits(), TASK_BYTES / UNIT);
        let frame = Frame {
            request_id: 1,
            message: Message::Pause {
                task_id: TaskId::generate(),
            },
        };
        let (mut writer, mut reader) = tokio::io::duplex(4096);
        protocol::write(&mut writer, &frame).await.unwrap();
        assert_eq!(
            read_file_frame(&budget, &mut reader, Some(task))
                .await
                .unwrap()
                .frame,
            frame
        );
    }

    #[tokio::test]
    async fn raw_speed_budget_holds_lease_until_the_frame_is_dropped() {
        use crate::node::core::protocol::SpeedControl;

        let budget = FrameBudget::default();
        let task = FrameBudget::task_limit();
        let frame = SpeedFrame {
            request_id: 1,
            control: SpeedControl::Busy {
                test_id: TaskId::parse("00112233445566778899aabbccddeeff").unwrap(),
            },
        };
        let encoded = frame.encode().unwrap();
        let units = (encoded.len() * 2).div_ceil(UNIT);
        let mut wire = (encoded.len() as u32).to_le_bytes().to_vec();
        wire.extend_from_slice(&encoded);

        let buffered = budget
            .read_raw(&mut &wire[..], Some(task.clone()))
            .await
            .unwrap();
        assert_eq!(buffered.bytes, encoded);
        assert_eq!(SpeedFrame::decode(&buffered.bytes).unwrap(), frame);
        assert_eq!(task.available_permits(), TASK_BYTES / UNIT - units);
        drop(buffered);
        assert_eq!(task.available_permits(), TASK_BYTES / UNIT);
    }
}
