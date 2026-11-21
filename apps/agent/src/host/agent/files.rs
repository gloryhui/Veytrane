//! IPC operations backed by the optional file capability.

use super::{Agent, Command, NodeId};
use crate::node::core::control::FileCommands;
use serde_json::{Value, json};
use veytrane_files::transfer::TransferService;

impl Agent {
    pub(super) async fn execute_files(
        &self,
        service: &TransferService,
        commands: FileCommands<'_>,
        command: Command,
    ) -> Result<Value, String> {
        match command {
            Command::Send { peer, paths } => {
                if paths.is_empty() || paths.len() > 128 || paths.iter().any(|p| !p.is_absolute()) {
                    return Err("发送路径必须是1..128个本机绝对路径".into());
                }
                commands.send_paths(NodeId::from_hex(&peer).map_err(|e| e.to_string())?, paths)?;
                Ok(json!({"accepted":true}))
            }
            Command::PreviewDirectory { peer, path } => {
                crate::node::core::drop_send::validate_paths(std::slice::from_ref(&path))
                    .map_err(|e| e.to_string())?;
                let request = commands
                    .preview_directory(NodeId::from_hex(&peer).map_err(|e| e.to_string())?, path)?;
                Ok(json!({"accepted":true,"request":request}))
            }
            Command::Pause { task } => {
                commands.pause_task(task)?;
                Ok(json!({"accepted":true}))
            }
            Command::Resume { peer, task } => {
                commands.resume_task(NodeId::from_hex(&peer).map_err(|e| e.to_string())?, task)?;
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
                commands.confirm_directory(
                    NodeId::from_hex(&peer).map_err(|e| e.to_string())?,
                    request.clone(),
                )?;
                self.file_management.forget_preview(&request).await;
                Ok(json!({"accepted":true}))
            }
            Command::CancelDirectory { request } => {
                commands.cancel_directory_preview(request.clone())?;
                self.file_management.forget_preview(&request).await;
                Ok(json!({"accepted":true}))
            }
            _ => Err("无法执行非文件命令".into()),
        }
    }
}
