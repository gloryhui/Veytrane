//! IPC operations backed by the optional file capability.

use super::{Agent, Command, FilePage, NodeId};
use serde_json::{Value, json};
use std::time::Duration;
use veytrane_files::task_model::TaskDiagnostic;
use veytrane_files::transfer::TransferService;

impl Agent {
    pub(super) async fn execute_files(
        &self,
        service: &TransferService,
        command: Command,
    ) -> Result<Value, String> {
        match command {
            Command::Send { peer, paths } => {
                if paths.is_empty() || paths.len() > 128 || paths.iter().any(|p| !p.is_absolute()) {
                    return Err("发送路径必须是1..128个本机绝对路径".into());
                }
                self.control
                    .send_paths(NodeId::from_hex(&peer).map_err(|e| e.to_string())?, paths)?;
                Ok(json!({"accepted":true}))
            }
            Command::PreviewDirectory { peer, path } => {
                crate::node::core::drop_send::validate_paths(std::slice::from_ref(&path))
                    .map_err(|e| e.to_string())?;
                let request = self
                    .control
                    .preview_directory(NodeId::from_hex(&peer).map_err(|e| e.to_string())?, path)?;
                Ok(json!({"accepted":true,"request":request}))
            }
            Command::Pause { task } => {
                self.control.pause_task(task)?;
                Ok(json!({"accepted":true}))
            }
            Command::Resume { peer, task } => {
                self.control
                    .resume_task(NodeId::from_hex(&peer).map_err(|e| e.to_string())?, task)?;
                Ok(json!({"accepted":true}))
            }
            Command::Cancel { task } => {
                service
                    .cancel_task(task)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(json!({"cancelled":true}))
            }
            Command::RemoveHistory { tasks } => {
                if tasks.is_empty() || tasks.len() > 128 {
                    return Err("每次只能清理1..128个已完成任务".into());
                }
                let removed = service
                    .remove_completed_history(tasks.into_iter().collect())
                    .map_err(|e| e.to_string())?;
                Ok(json!({"removed":removed}))
            }
            Command::DeleteReceivedFile { task } => {
                service
                    .delete_completed_receive_file(&task)
                    .map_err(|e| e.to_string())?;
                Ok(json!({"deleted":true}))
            }
            Command::TaskLocation { task } => {
                let path = service
                    .reveal_task(&veytrane_files::task_location::Selection::Task(task))
                    .map_err(|e| e.to_string())?;
                Ok(json!({"path":path}))
            }
            Command::ConfirmDirectory { peer, request } => {
                self.control.confirm_directory(
                    NodeId::from_hex(&peer).map_err(|e| e.to_string())?,
                    request.clone(),
                )?;
                self.previews.lock().await.retain(|(_, p)| p.id != request);
                Ok(json!({"accepted":true}))
            }
            Command::CancelDirectory { request } => {
                self.control.cancel_directory_preview(request.clone())?;
                self.previews.lock().await.retain(|(_, p)| p.id != request);
                Ok(json!({"accepted":true}))
            }
            _ => Err("无法执行非文件命令".into()),
        }
    }

    pub(super) async fn file_page(&self, offset: usize) -> Result<FilePage, String> {
        let snapshot = self
            .file_service
            .as_ref()
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
