//! Admission and guarded execution for file commands owned by a node session.

use crate::task_model::TaskId;
use crate::transfer::{FileCommand, FileCommandResult, TransferService};
use std::future::Future;
use std::sync::Arc;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use veytrane_node::live_authorization::LiveAuthorization;

const SELECTION_JOBS: usize = 64;
const CONTINUATION_JOBS: usize = 3;

pub enum SessionFileCommand {
    Transfer(FileCommand),
    Pause(TaskId),
    CancelDirectoryPreview(TaskId),
}

impl SessionFileCommand {
    pub fn unavailable(&self) -> &'static str {
        match self {
            Self::Transfer(FileCommand::SendPaths { .. }) => {
                "传输命令队列已满或会话已关闭；拖入项尚未提交"
            }
            Self::Transfer(FileCommand::PreviewDirectory { .. }) => {
                "目录预览命令队列已满或会话已关闭"
            }
            Self::Transfer(FileCommand::ConfirmDirectory { .. }) => {
                "目录确认命令队列已满或会话已关闭"
            }
            Self::CancelDirectoryPreview(_) => "目录取消命令队列已满或会话已关闭",
            _ => "传输命令队列已满或会话已关闭",
        }
    }
}

pub struct FileCommandJobs {
    selection_jobs: Arc<Semaphore>,
    continuation_jobs: Arc<Semaphore>,
}

impl Default for FileCommandJobs {
    fn default() -> Self {
        Self {
            selection_jobs: Arc::new(Semaphore::new(SELECTION_JOBS)),
            continuation_jobs: Arc::new(Semaphore::new(CONTINUATION_JOBS)),
        }
    }
}

impl FileCommandJobs {
    pub fn prepare(
        &self,
        service: TransferService,
        command: FileCommand,
    ) -> Result<PreparedFileCommand, PrepareError> {
        let preview_request = command.preview_request().cloned();
        let job_pool = if matches!(&command, FileCommand::Resume { .. }) {
            &self.continuation_jobs
        } else {
            &self.selection_jobs
        };
        let Ok(permit) = job_pool.clone().try_acquire_owned() else {
            return Err(PrepareError::QueueFull);
        };
        if let Some(request) = &preview_request
            && let Err(error) = service.begin_preview(command.peer(), request.clone())
        {
            return Err(PrepareError::BeginPreview {
                request: request.clone(),
                message: error.to_string(),
            });
        }
        Ok(PreparedFileCommand {
            service,
            command,
            preview_request,
            _permit: permit,
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PrepareError {
    #[error("文件命令排队已达上限，请稍后重试")]
    QueueFull,
    #[error("{message}")]
    BeginPreview { request: TaskId, message: String },
}

pub struct PreparedFileCommand {
    service: TransferService,
    command: FileCommand,
    preview_request: Option<TaskId>,
    _permit: OwnedSemaphorePermit,
}

impl PreparedFileCommand {
    pub async fn run<F, Fut>(
        self,
        live: LiveAuthorization,
        connection: Option<quinn::Connection>,
        mut project: F,
    ) where
        F: FnMut(Result<FileCommandResult, String>) -> Fut,
        Fut: Future<Output = ()>,
    {
        let Self {
            service,
            command,
            preview_request,
            _permit,
        } = self;
        let result = live
            .guard(false, async {
                let result = service
                    .execute_command(connection.as_ref(), command)
                    .await?;
                project(Ok(result)).await;
                Ok(())
            })
            .await;
        if let Err(error) = result {
            if let Some(request) = &preview_request {
                service.cancel_preview(request);
            }
            project(Err(error.to_string())).await;
        }
    }
}
