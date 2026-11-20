//! Bounded application frame memory. Reservations precede allocation and remain
//! attached to decoded frames until the task finishes processing them.
use crate::p2pf::MAX_FRAME_BYTES;
use std::sync::Arc;
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    sync::{OwnedSemaphorePermit, Semaphore},
};
use veytrane_transport::error::{Error, Result};

pub const UNIT: usize = 64 * 1024;
pub const GLOBAL_BYTES: usize = 64 * 1024 * 1024;
pub const DATA_BYTES: usize = 56 * 1024 * 1024;
pub const TASK_BYTES: usize = 8 * 1024 * 1024;

fn failure(message: &str) -> Error {
    Error::Protocol(message.into())
}

fn local_error(error: impl std::fmt::Display) -> Error {
    Error::Protocol(error.to_string())
}

#[derive(Clone)]
pub struct FrameBudget {
    all: Arc<Semaphore>,
    data: Arc<Semaphore>,
}
impl Default for FrameBudget {
    fn default() -> Self {
        Self {
            all: Arc::new(Semaphore::new(GLOBAL_BYTES / UNIT)),
            data: Arc::new(Semaphore::new(DATA_BYTES / UNIT)),
        }
    }
}
pub struct FrameLease {
    _data: Option<OwnedSemaphorePermit>,
    _all: OwnedSemaphorePermit,
    _task: Option<OwnedSemaphorePermit>,
}
pub struct BufferedBytes {
    pub bytes: Vec<u8>,
    pub _lease: FrameLease,
}
impl FrameBudget {
    pub fn task_limit() -> Arc<Semaphore> {
        Arc::new(Semaphore::new(TASK_BYTES / UNIT))
    }
    async fn reserve(&self, length: usize, task: Option<Arc<Semaphore>>) -> Result<FrameLease> {
        if length == 0 || length > MAX_FRAME_BYTES as usize {
            return Err(failure("桌面帧长度超出资源上限"));
        }
        // Raw bytes and decoded payload can coexist during deserialization.
        let units = (length * 2).div_ceil(UNIT) as u32;
        let task = match task {
            Some(task) => Some(task.acquire_many_owned(units).await.map_err(local_error)?),
            None => None,
        };
        // Large frames cannot consume the small-control reserve. Acquire this
        // before the global budget, so a waiting data stream holds no global bytes.
        let data = if length >= UNIT {
            Some(
                self.data
                    .clone()
                    .acquire_many_owned(units)
                    .await
                    .map_err(local_error)?,
            )
        } else {
            None
        };
        let all = self
            .all
            .clone()
            .acquire_many_owned(units)
            .await
            .map_err(local_error)?;
        Ok(FrameLease {
            _data: data,
            _all: all,
            _task: task,
        })
    }
    pub async fn read_raw<R: AsyncRead + Unpin>(
        &self,
        reader: &mut R,
        task: Option<Arc<Semaphore>>,
    ) -> Result<BufferedBytes> {
        let length = reader.read_u32_le().await? as usize;
        let lease = self.reserve(length, task).await?;
        let mut bytes = vec![0; length];
        reader.read_exact(&mut bytes).await?;
        Ok(BufferedBytes {
            bytes,
            _lease: lease,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn global_data_cap_leaves_control_room_and_cancel_returns_every_permit() {
        let budget = FrameBudget::default();
        let mut held = Vec::new();
        for _ in 0..7 {
            held.push(
                budget
                    .reserve(MAX_FRAME_BYTES as usize, None)
                    .await
                    .unwrap(),
            );
        }
        assert_eq!(budget.data.available_permits(), 0);
        let waiting = budget.clone();
        let blocked =
            tokio::spawn(async move { waiting.reserve(MAX_FRAME_BYTES as usize, None).await });
        tokio::task::yield_now().await;
        assert!(!blocked.is_finished());
        let control = budget.reserve(1024, None).await.unwrap();
        assert_eq!(
            budget.all.available_permits(),
            (GLOBAL_BYTES - DATA_BYTES) / UNIT - 1
        );
        blocked.abort();
        let _ = blocked.await;
        drop(control);
        drop(held);
        assert_eq!(budget.all.available_permits(), GLOBAL_BYTES / UNIT);
        assert_eq!(budget.data.available_permits(), DATA_BYTES / UNIT);
    }
    #[tokio::test]
    async fn per_task_cap_and_invalid_header_do_not_allocate_or_leak() {
        let budget = FrameBudget::default();
        let task = FrameBudget::task_limit();
        let held = budget
            .reserve(MAX_FRAME_BYTES as usize, Some(task.clone()))
            .await
            .unwrap();
        let second = budget.reserve(1, Some(task.clone()));
        tokio::pin!(second);
        std::future::poll_fn(|cx| {
            assert!(std::future::Future::poll(second.as_mut(), cx).is_pending());
            std::task::Poll::Ready(())
        })
        .await;
        drop(held);
        let next = second.await.unwrap();
        drop(next);
        assert_eq!(task.available_permits(), TASK_BYTES / UNIT);
        let mut reader = &(MAX_FRAME_BYTES + 1).to_le_bytes()[..];
        assert!(budget.read_raw(&mut reader, Some(task)).await.is_err());
        assert_eq!(budget.all.available_permits(), GLOBAL_BYTES / UNIT);
    }
}
