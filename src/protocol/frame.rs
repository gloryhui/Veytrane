//! 长度前缀帧：`[u32 小端长度][postcard 载荷]`。
//!
//! QUIC 的流是字节流，本身不保留消息边界，所以自己做一层定长头。

use tokio::io::{AsyncRead, AsyncWrite};

use crate::error::{Error, Result};
use crate::protocol::message::ControlMessage;
pub use crate::protocol::raw_frame::{
    MAX_FRAME_LEN, read_raw_frame, read_raw_frame_limited, write_raw_frame, write_raw_frame_limited,
};

/// 写一帧控制消息。
pub async fn write_frame<W>(writer: &mut W, message: &ControlMessage) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    let payload = message.encode()?;
    if payload.len() > MAX_FRAME_LEN as usize {
        return Err(Error::Protocol(format!(
            "{} 帧过大：{} 字节，上限 {MAX_FRAME_LEN}",
            message.kind(),
            payload.len()
        )));
    }
    write_raw_frame(writer, &payload).await
}

/// 读一帧控制消息。
pub async fn read_frame<R>(reader: &mut R) -> Result<Option<ControlMessage>>
where
    R: AsyncRead + Unpin,
{
    match read_raw_frame(reader).await? {
        Some(payload) => Ok(Some(ControlMessage::decode(&payload)?)),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::message::PROTOCOL_VERSION;
    use tokio::io::AsyncWriteExt;

    #[tokio::test]
    async fn 多帧往返() {
        let (mut a, mut b) = tokio::io::duplex(1024);

        let sent = vec![
            ControlMessage::Ready,
            ControlMessage::RequestChunk { index: 7 },
            ControlMessage::Chunk {
                index: 7,
                data: vec![0x5a; 5000],
            },
            ControlMessage::KeepAlive,
        ];

        let sent_to_peer = sent.clone();

        let writer = tokio::spawn(async move {
            for message in &sent_to_peer {
                write_frame(&mut a, message).await.unwrap();
            }
        });

        for expected in &sent {
            let got = read_frame(&mut b).await.unwrap().unwrap();
            assert_eq!(&got, expected);
        }

        writer.await.unwrap();
    }

    #[tokio::test]
    async fn 干净关闭返回_none() {
        let (a, mut b) = tokio::io::duplex(64);
        drop(a);
        assert!(read_frame(&mut b).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn 半截帧报错() {
        let (mut a, mut b) = tokio::io::duplex(64);
        // 声称有 100 字节，实际只给 3 字节就关闭。
        tokio::spawn(async move {
            a.write_all(&100u32.to_le_bytes()).await.unwrap();
            a.write_all(&[1, 2, 3]).await.unwrap();
            drop(a);
        });
        let err = read_frame(&mut b).await.unwrap_err();
        assert!(
            matches!(err, Error::Protocol(_)),
            "应报协议错误，实际 {err:?}"
        );
    }

    #[tokio::test]
    async fn 超大长度头被拒绝() {
        let (mut a, mut b) = tokio::io::duplex(64);
        tokio::spawn(async move {
            a.write_all(&u32::MAX.to_le_bytes()).await.unwrap();
            // 停在把长度头写完的状态
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        });
        let err = read_frame(&mut b).await.unwrap_err();
        assert!(matches!(err, Error::Protocol(_)), "实际 {err:?}");
    }

    #[tokio::test]
    async fn 版本号字段能穿过帧() {
        let (mut a, mut b) = tokio::io::duplex(1024);
        let message = ControlMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
            node_id: crate::identity::NodeId::from_hex("00000000000000000000000000000001").unwrap(),
            public_key: [3u8; 32],
            nonce: [4u8; 32],
        };
        let clone = message.clone();
        tokio::spawn(async move { write_frame(&mut a, &message).await.unwrap() });
        assert_eq!(read_frame(&mut b).await.unwrap().unwrap(), clone);
    }
}
