//! Memory-only throughput measurement on an already claimed stream.

use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use veytrane_transport::TRANSFER_IDLE_TIMEOUT;
use veytrane_transport::error::{Error, Result};

const SPEEDTEST_PATTERN: u8 = 0xa5;

/// Writes data until the measurement deadline or a stream error.
pub async fn send_memory<W, F>(
    writer: &mut W,
    duration: Duration,
    block_size: usize,
    mut progress: F,
) -> Result<(u64, Duration)>
where
    W: AsyncWrite + Unpin,
    F: FnMut(u64, Duration),
{
    let buffer = vec![SPEEDTEST_PATTERN; block_size];
    let start = tokio::time::Instant::now();
    let deadline = start + duration;
    let mut bytes = 0u64;
    while tokio::time::Instant::now() < deadline {
        let idle_deadline = (tokio::time::Instant::now() + TRANSFER_IDLE_TIMEOUT).min(deadline);
        let n = match tokio::time::timeout_at(idle_deadline, writer.write(&buffer)).await {
            Ok(result) => result?,
            Err(_) if tokio::time::Instant::now() >= deadline => break,
            Err(_) => return Err(Error::Transport("测速数据流写入空闲超时".into())),
        };
        if n == 0 {
            return Err(std::io::Error::from(std::io::ErrorKind::WriteZero).into());
        }
        bytes = bytes
            .checked_add(n as u64)
            .ok_or_else(|| Error::Protocol("测速字节计数超限".into()))?;
        progress(bytes, start.elapsed());
        tokio::task::yield_now().await;
    }
    Ok((bytes, start.elapsed()))
}

/// Measures the bytes read before EOF and rejects an empty measurement.
pub async fn receive_memory<R, F>(
    reader: &mut R,
    block_size: usize,
    mut progress: F,
) -> Result<(u64, Duration)>
where
    R: AsyncRead + Unpin,
    F: FnMut(u64, Duration),
{
    let mut buffer = vec![0; block_size];
    let start = tokio::time::Instant::now();
    let mut bytes = 0u64;
    loop {
        let n = tokio::time::timeout(TRANSFER_IDLE_TIMEOUT, reader.read(&mut buffer))
            .await
            .map_err(|_| Error::Transport("测速数据流读取空闲超时".into()))??;
        if n == 0 {
            break;
        }
        bytes = bytes
            .checked_add(n as u64)
            .ok_or_else(|| Error::Protocol("测速字节计数超限".into()))?;
        progress(bytes, start.elapsed());
    }
    if bytes == 0 {
        return Err(Error::Protocol("测速数据流没有收到 payload".into()));
    }
    Ok((bytes, start.elapsed()))
}
