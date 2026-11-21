//! Agent-facing file management snapshots and preview cache.

use crate::incremental::Preview;
use crate::task_model::{TaskDiagnostic, TaskId};
use crate::transfer::TransferService;
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

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
