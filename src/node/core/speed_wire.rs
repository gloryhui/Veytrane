//! Root compatibility path plus old-P2PF interop tests.

pub(crate) use veytrane_speed::wire::SpeedFrame;

#[cfg(test)]
mod tests {
    use super::SpeedFrame;
    use crate::error::Error;
    use crate::identity::NodeId;
    use crate::node::core::protocol::{
        Frame, MAX_FRAME_BYTES, Message, SpeedControl, SpeedDirection, SpeedLease,
    };
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
        zero[5] = 0;
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
