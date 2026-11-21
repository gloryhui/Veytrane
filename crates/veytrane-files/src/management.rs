//! Agent-facing file management snapshots and preview cache.

use crate::command_jobs::SessionFileCommand;
use crate::incremental::Preview;
use crate::management_command::FileManagementCommand;
use crate::task_location::Selection;
use crate::task_model::{TaskDiagnostic, TaskId};
use crate::transfer::{FileCommand, TransferService};
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use veytrane_transport::identity::NodeId;

#[derive(Default)]
pub struct FilePage {
    pub tasks: Vec<Value>,
    pub queue: Option<Value>,
    pub total: usize,
    pub previews: Vec<Value>,
}

#[derive(Default)]
pub struct FileManagementState {
    previews: Mutex<VecDeque<(Instant, Preview)>>,
}

impl FileManagementState {
    pub async fn execute(
        &self,
        service: &TransferService,
        command: FileManagementCommand,
        enqueue: impl FnOnce(SessionFileCommand) -> Result<(), String>,
    ) -> Result<Value, String> {
        use FileManagementCommand as Management;
        use SessionFileCommand::Transfer;

        match command {
            Management::Send { peer, paths } => {
                if paths.is_empty()
                    || paths.len() > crate::path_batch::MAX_PATHS
                    || paths.iter().any(|path| !path.is_absolute())
                {
                    return Err("发送路径必须是1..128个本机绝对路径".into());
                }
                let peer = NodeId::from_hex(&peer).map_err(|error| error.to_string())?;
                let sources =
                    crate::drop_send::normalize(paths).map_err(|error| error.to_string())?;
                enqueue(Transfer(FileCommand::SendPaths { peer, sources }))?;
                Ok(json!({"accepted":true}))
            }
            Management::PreviewDirectory { peer, path } => {
                crate::drop_send::validate_paths(std::slice::from_ref(&path))
                    .map_err(|error| error.to_string())?;
                let peer = NodeId::from_hex(&peer).map_err(|error| error.to_string())?;
                let request = TaskId::generate();
                enqueue(Transfer(FileCommand::PreviewDirectory {
                    peer,
                    source: path,
                    request: request.clone(),
                }))?;
                Ok(json!({"accepted":true,"request":request}))
            }
            Management::Pause { task } => {
                enqueue(SessionFileCommand::Pause(task))?;
                Ok(json!({"accepted":true}))
            }
            Management::Resume { peer, task } => {
                let peer = NodeId::from_hex(&peer).map_err(|error| error.to_string())?;
                enqueue(Transfer(FileCommand::Resume { peer, task }))?;
                Ok(json!({"accepted":true}))
            }
            Management::Cancel { task } => {
                service
                    .cancel_task(task)
                    .await
                    .map_err(|error| error.to_string())?;
                Ok(json!({"cancelled":true}))
            }
            Management::RemoveHistory { tasks } => {
                if tasks.is_empty() || tasks.len() > 128 {
                    return Err("每次只能清理1..128个已完成任务".into());
                }
                let removed = service
                    .remove_completed_history(tasks.into_iter().collect())
                    .map_err(|error| error.to_string())?;
                Ok(json!({"removed":removed}))
            }
            Management::DeleteReceivedFile { task } => {
                service
                    .delete_completed_receive_file(&task)
                    .map_err(|error| error.to_string())?;
                Ok(json!({"deleted":true}))
            }
            Management::TaskLocation { task } => {
                let path = service
                    .reveal_task(&Selection::Task(task))
                    .map_err(|error| error.to_string())?;
                Ok(json!({"path":path}))
            }
            Management::ConfirmDirectory { peer, request } => {
                let peer = NodeId::from_hex(&peer).map_err(|error| error.to_string())?;
                enqueue(Transfer(FileCommand::ConfirmDirectory {
                    peer,
                    request: request.clone(),
                }))?;
                self.forget_preview(&request).await;
                Ok(json!({"accepted":true}))
            }
            Management::CancelDirectory { request } => {
                enqueue(SessionFileCommand::CancelDirectoryPreview(request.clone()))?;
                self.forget_preview(&request).await;
                Ok(json!({"accepted":true}))
            }
        }
    }

    pub async fn capture_preview(&self, preview: Preview) {
        let mut previews = self.previews.lock().await;
        previews.retain(|(_, cached)| cached.id != preview.id);
        if previews.len() == 32 {
            previews.pop_front();
        }
        previews.push_back((Instant::now(), preview));
    }

    pub async fn forget_preview(&self, id: &TaskId) {
        self.previews
            .lock()
            .await
            .retain(|(_, preview)| &preview.id != id);
    }

    pub async fn page(
        &self,
        service: Option<&TransferService>,
        offset: usize,
    ) -> Result<FilePage, String> {
        let snapshot = service
            .map(|service| service.file_snapshot())
            .transpose()
            .map_err(|error| error.to_string())?;
        let mut page = FilePage::default();
        if let Some(snapshot) = snapshot {
            page.total = snapshot.tasks.len();
            page.queue = Some(json!({
                "pending": snapshot.queue.pending,
                "active_files": snapshot.queue.active_files,
                "limit": snapshot.queue.limit,
                "converging": snapshot.queue.converging,
            }));
            page.tasks = snapshot
                .tasks
                .iter()
                .skip(offset)
                .take(128)
                .map(|task| {
                    let diagnostic = task.diagnostic.map(TaskDiagnostic::safe_message);
                    json!({
                        "id": task.id,
                        "group": task.group,
                        "peer": task.peer.to_hex(),
                        "name": task.name.chars().take(256).collect::<String>(),
                        "name_truncated": task.name.chars().count() > 256,
                        "state": task.state,
                        "direction": task.direction,
                        "total": task.total,
                        "confirmed": task.confirmed,
                        "rate": task.rate,
                        "diagnostic": diagnostic,
                        "retryable": task.retryable(),
                        "can_pause": task.can_pause(),
                        "can_continue": task.can_continue(),
                        "can_delete_file": task.can_delete_file,
                    })
                })
                .collect();
        }
        let mut previews = self.previews.lock().await;
        previews.retain(|(created, _)| created.elapsed() < Duration::from_secs(300));
        page.previews = previews
            .iter()
            .map(|(_, preview)| {
                let mut value = serde_json::to_value(preview).expect("preview DTO");
                value["peer"] = json!(preview.peer.to_hex());
                value.as_object_mut().unwrap().remove("source");
                value["name"] = json!(
                    preview
                        .source
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .chars()
                        .take(128)
                        .collect::<String>()
                );
                value
            })
            .collect();
        Ok(page)
    }
}
