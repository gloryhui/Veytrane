//! Bounded file-task facts. Presentation labels, selection and filtering stay above this layer.

use veytrane_transport::identity::NodeId;

use crate::queue::QueueMetrics;
use crate::space_budget::Summary;
use crate::task_model::{TaskDiagnostic, TaskDirection, TaskId, TaskRecord, TaskState};

#[derive(Clone, Debug)]
pub struct FileTaskSnapshot {
    pub id: TaskId,
    pub group: Option<TaskId>,
    pub name: String,
    pub peer: NodeId,
    pub direction: TaskDirection,
    pub state: TaskState,
    pub total: u64,
    pub confirmed: u64,
    pub rate: f64,
    pub diagnostic: Option<TaskDiagnostic>,
    pub auto_waiting: bool,
    pub can_delete_file: bool,
}

impl FileTaskSnapshot {
    pub fn from_record(record: &TaskRecord, rate: f64, auto_waiting: bool) -> Self {
        let total = record.manifest_identity().total_bytes();
        Self {
            id: record.task_id().clone(),
            group: record.group_id().cloned(),
            name: record
                .relative_path()
                .map(str::to_owned)
                .unwrap_or_else(|| {
                    record
                        .local_path()
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned()
                }),
            peer: NodeId::from_hex(record.peer_id().as_str()).expect("validated persisted peer"),
            direction: record.direction(),
            state: record.state(),
            total,
            confirmed: if record.state() == TaskState::Completed {
                total
            } else {
                record
                    .progress_hint()
                    .map_or(0, |progress| progress.verified_bytes())
                    .min(total)
            },
            rate: if record.state() == TaskState::Transferring {
                rate.max(0.)
            } else {
                0.
            },
            diagnostic: record.diagnostic(),
            auto_waiting,
            can_delete_file: record.direction() == TaskDirection::Receive
                && record.state() == TaskState::Completed
                && record
                    .file_details()
                    .is_some_and(|details| details.receipt_committed),
        }
    }
}

#[derive(Clone, Debug)]
pub struct FileSnapshot {
    pub space: Summary,
    pub tasks: Vec<FileTaskSnapshot>,
    pub queue: QueueMetrics,
}
