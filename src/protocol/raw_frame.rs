//! Neutral length-prefixed frame I/O: `[u32 LE length][payload]`.
//!
//! This leaf module does not import the control-message enum. Control messages,
//! identity handshakes and signaling all share this byte envelope.

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::error::{Error, Result};

/// 单帧上限 16 MiB。分片大小上限也是 16 MiB，留出 postcard 自身的编码开销。
pub const MAX_FRAME_LEN: u32 = 16 * 1024 * 1024 + 4096;

/// 写一帧原始载荷并 flush。
pub async fn write_raw_frame<W>(writer: &mut W, payload: &[u8]) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    write_raw_frame_limited(writer, payload, MAX_FRAME_LEN).await
}

/// 写一帧原始载荷，并指定这一层协议自己的长度上限。
pub async fn write_raw_frame_limited<W>(writer: &mut W, payload: &[u8], max_len: u32) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    let len = payload.len();
    if len > max_len as usize {
        return Err(Error::Protocol(format!(
            "帧过大：{len} 字节，上限 {max_len}"
        )));
    }
    writer.write_all(&(len as u32).to_le_bytes()).await?;
    writer.write_all(payload).await?;
    writer.flush().await?;
    Ok(())
}

/// 读一帧原始载荷。
///
/// 对端干净关闭（连长度头都没读到就 EOF）时返回 `Ok(None)`；
/// 只读到一半就断开视为协议错误。
pub async fn read_raw_frame<R>(reader: &mut R) -> Result<Option<Vec<u8>>>
where
    R: AsyncRead + Unpin,
{
    read_raw_frame_limited(reader, MAX_FRAME_LEN).await
}

/// 读一帧原始载荷，并指定这一层协议自己的长度上限。
///
/// 长度头必须先于分配被检查：超限直接报错，绝不按对端声称的长度去 `Vec` 分配。
pub async fn read_raw_frame_limited<R>(reader: &mut R, max_len: u32) -> Result<Option<Vec<u8>>>
where
    R: AsyncRead + Unpin,
{
    let mut len_bytes = [0u8; 4];
    let mut filled = 0usize;
    while filled < len_bytes.len() {
        match reader.read(&mut len_bytes[filled..]).await {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err.into()),
        }
    }

    if filled == 0 {
        return Ok(None);
    }
    if filled < len_bytes.len() {
        return Err(Error::Protocol(format!(
            "帧长度头只读到 {filled}/4 字节连接就断了"
        )));
    }

    let len = u32::from_le_bytes(len_bytes);
    if len > max_len {
        return Err(Error::Protocol(format!(
            "帧长度 {len} 超过上限 {max_len}，可能不是本协议的流"
        )));
    }

    let mut payload = vec![0u8; len as usize];
    reader
        .read_exact(&mut payload)
        .await
        .map_err(|err| match err.kind() {
            std::io::ErrorKind::UnexpectedEof => Error::Protocol("帧读到一半连接就断了".into()),
            _ => err.into(),
        })?;

    Ok(Some(payload))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    /// 长度头读到 1~3 字节就断流：必须是协议错误，不能当成「对端干净关闭」。
    #[tokio::test]
    async fn 半截长度头报错而不是干净关闭() {
        for partial in 1..4usize {
            let (mut a, mut b) = tokio::io::duplex(64);
            let prefix = vec![0xabu8; partial];
            tokio::spawn(async move {
                a.write_all(&prefix).await.unwrap();
                drop(a);
            });
            let got = read_raw_frame(&mut b).await;
            match got {
                Err(Error::Protocol(_)) => {}
                other => panic!("{partial} 字节长度头应报协议错误，实际 {other:?}"),
            }
        }
    }

    /// 0 字节 EOF（连长度头都没开始）才是干净关闭。
    #[tokio::test]
    async fn 零字节长度头是干净关闭() {
        let (a, mut b) = tokio::io::duplex(64);
        drop(a);
        assert!(read_raw_frame(&mut b).await.unwrap().is_none());

        // 长度头本身就是全 0 是合法的空帧，不能被误判成关闭。
        let (mut a, mut b) = tokio::io::duplex(64);
        tokio::spawn(async move {
            a.write_all(&0u32.to_le_bytes()).await.unwrap();
            a.flush().await.unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        });
        assert_eq!(read_raw_frame(&mut b).await.unwrap(), Some(Vec::new()));
    }

    #[tokio::test]
    async fn 自定义上限比通用上限更严() {
        let payload = vec![0u8; 1024];
        let (mut a, mut b) = tokio::io::duplex(4096);

        let outgoing = payload.clone();
        tokio::spawn(async move {
            write_raw_frame(&mut a, &outgoing).await.unwrap();
        });
        assert!(
            read_raw_frame_limited(&mut b, 4096)
                .await
                .unwrap()
                .is_some()
        );

        let (mut a, mut b) = tokio::io::duplex(4096);
        tokio::spawn(async move {
            a.write_all(&1024u32.to_le_bytes()).await.unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        });
        let err = read_raw_frame_limited(&mut b, 64).await.unwrap_err();
        assert!(matches!(err, Error::Protocol(_)), "实际 {err:?}");

        let (mut a, _b) = tokio::io::duplex(4096);
        let err = write_raw_frame_limited(&mut a, &payload, 64)
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Protocol(_)), "实际 {err:?}");
    }
}
