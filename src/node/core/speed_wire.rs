//! Narrow speed-control envelope.
//!
//! Bytes remain compatible with the P2PF file frame:
//! `P2PF\x01 | request_id | postcard Message::Speed (tag 7) | SpeedControl`.
//! Speed execution no longer decodes the full file `Message` enum.

use crate::error::{Error, Result};
use crate::node::core::protocol::{MAGIC, MAX_FRAME_BYTES, SpeedControl};

const SPEED_MESSAGE_TAG: u32 = 7;

fn invalid(message: &str) -> Error {
    Error::Protocol(message.into())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SpeedFrame {
    pub(crate) request_id: u64,
    pub(crate) control: SpeedControl,
}

impl SpeedFrame {
    pub(crate) fn encode(&self) -> Result<Vec<u8>> {
        self.validate()?;
        let mut bytes = MAGIC.to_vec();
        bytes.extend_from_slice(&postcard::to_allocvec(&self.request_id)?);
        bytes.extend_from_slice(&postcard::to_allocvec(&SPEED_MESSAGE_TAG)?);
        bytes.extend_from_slice(&postcard::to_allocvec(&self.control)?);
        if bytes.len() > MAX_FRAME_BYTES as usize {
            return Err(invalid("文件帧超限"));
        }
        Ok(bytes)
    }

    pub(crate) fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > MAX_FRAME_BYTES as usize {
            return Err(invalid("文件帧超限"));
        }
        let payload = bytes
            .strip_prefix(MAGIC)
            .ok_or_else(|| invalid("对端不支持文件协议"))?;
        let (request_id, rest): (u64, &[u8]) = postcard::take_from_bytes(payload)?;
        let (tag, rest): (u32, &[u8]) = postcard::take_from_bytes(rest)?;
        if tag != SPEED_MESSAGE_TAG {
            return Err(invalid("非法测速控制消息"));
        }
        let (control, trailing): (SpeedControl, &[u8]) = postcard::take_from_bytes(rest)?;
        if !trailing.is_empty() {
            return Err(invalid("文件帧含多余字节"));
        }
        let frame = Self {
            request_id,
            control,
        };
        frame.validate()?;
        Ok(frame)
    }

    fn validate(&self) -> Result<()> {
        if self.request_id == 0 {
            return Err(invalid("文件请求编号非法"));
        }
        self.control.validate()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::NodeId;
    use crate::node::core::protocol::{Frame, Message, SpeedDirection, SpeedLease};
    use crate::node::core::task_model::TaskId;

    fn task() -> TaskId {
        TaskId::parse("00112233445566778899aabbccddeeff").unwrap()
    }

    #[test]
    fn fixed_speed_bytes_and_old_new_interop() {
        let control = SpeedControl::Busy { test_id: task() };
        let frame = SpeedFrame {
            request_id: 1,
            control: control.clone(),
        };
        let mut expected = b"P2PF\x01\x01\x07\x02\x20".to_vec();
        expected.extend_from_slice(task().as_str().as_bytes());
        assert_eq!(frame.encode().unwrap(), expected);

        let old = Frame {
            request_id: 1,
            message: Message::Speed(control.clone()),
        };
        assert_eq!(frame.encode().unwrap(), old.encode().unwrap());
        assert_eq!(SpeedFrame::decode(&old.encode().unwrap()).unwrap(), frame);
        assert_eq!(Frame::decode(&frame.encode().unwrap()).unwrap(), old);
    }

    #[test]
    fn leased_speed_control_matches_old_frame_bytes() {
        let control = SpeedControl::Result {
            lease: SpeedLease {
                test_id: task(),
                owner: NodeId::from_hex("11111111111111111111111111111111").unwrap(),
                direction: SpeedDirection::Download,
                seconds: 60,
                stream_token: [0x44; 16],
            },
            bytes: 123,
            elapsed_ms: 456,
        };
        let new = SpeedFrame {
            request_id: 2,
            control: control.clone(),
        };
        let old = Frame {
            request_id: 2,
            message: Message::Speed(control),
        };
        assert_eq!(new.encode().unwrap(), old.encode().unwrap());
        assert_eq!(SpeedFrame::decode(&old.encode().unwrap()).unwrap(), new);
        assert_eq!(Frame::decode(&new.encode().unwrap()).unwrap(), old);
    }

    #[test]
    fn speed_wire_rejects_other_tags_magic_trailing_and_bad_numbers() {
        let base = SpeedFrame {
            request_id: 1,
            control: SpeedControl::Busy { test_id: task() },
        }
        .encode()
        .unwrap();

        let mut wrong_magic = base.clone();
        wrong_magic[3] = b'D';
        assert!(matches!(
            SpeedFrame::decode(&wrong_magic),
            Err(Error::Protocol(_))
        ));

        let file_frame = Frame {
            request_id: 1,
            message: Message::ResumeTask { task_id: task() },
        }
        .encode()
        .unwrap();
        assert!(matches!(
            SpeedFrame::decode(&file_frame),
            Err(Error::Protocol(_))
        ));

        let mut zero = base.clone();
        zero[MAGIC.len()] = 0;
        assert!(matches!(SpeedFrame::decode(&zero), Err(Error::Protocol(_))));

        let mut trailing = base;
        trailing.push(0);
        assert!(matches!(
            SpeedFrame::decode(&trailing),
            Err(Error::Protocol(_))
        ));

        assert!(matches!(
            SpeedFrame {
                request_id: 0,
                control: SpeedControl::Busy { test_id: task() },
            }
            .encode(),
            Err(Error::Protocol(_))
        ));
    }

    #[test]
    fn speed_wire_rejects_invalid_seconds_and_oversize_bytes() {
        assert!(matches!(
            SpeedFrame {
                request_id: 1,
                control: SpeedControl::Request {
                    test_id: task(),
                    direction: SpeedDirection::Upload,
                    seconds: 59,
                },
            }
            .encode(),
            Err(Error::Protocol(_))
        ));
        let oversize = vec![0u8; MAX_FRAME_BYTES as usize + 1];
        assert!(matches!(
            SpeedFrame::decode(&oversize),
            Err(Error::Protocol(_))
        ));
    }
}
