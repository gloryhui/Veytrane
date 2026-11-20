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

    pub fn retryable(&self) -> bool {
        self.diagnostic.is_some_and(TaskDiagnostic::is_retryable)
    }

    pub fn can_pause(&self) -> bool {
        self.auto_waiting
            || matches!(
                self.state,
                TaskState::Queued
                    | TaskState::Connecting
                    | TaskState::Negotiating
                    | TaskState::Transferring
            )
    }

    pub fn can_continue(&self) -> bool {
        !self.auto_waiting
            && (matches!(self.state, TaskState::Paused | TaskState::Interrupted)
                || (self.state == TaskState::Failed && self.retryable()))
    }
}

#[derive(Clone, Debug)]
pub struct FileSnapshot {
    pub space: Summary,
    pub tasks: Vec<FileTaskSnapshot>,
    pub queue: QueueMetrics,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task_model::{TaskDiagnostic, TaskErrorCode};

    fn fact(
        state: TaskState,
        diagnostic: Option<TaskDiagnostic>,
        auto_waiting: bool,
    ) -> FileTaskSnapshot {
        FileTaskSnapshot {
            id: TaskId::parse("22222222222222222222222222222222").unwrap(),
            group: None,
            name: "fixture.bin".into(),
            peer: NodeId::from_hex("11111111111111111111111111111111").unwrap(),
            direction: TaskDirection::Receive,
            state,
            total: 1,
            confirmed: 0,
            rate: 0.,
            diagnostic,
            auto_waiting,
            can_delete_file: false,
        }
    }

    #[test]
    fn actionability_is_derived_from_file_state_without_presentation_labels() {
        let transferring = fact(TaskState::Transferring, None, false);
        assert!(transferring.can_pause());
        assert!(!transferring.can_continue());

        let paused = fact(TaskState::Paused, None, false);
        assert!(!paused.can_pause());
        assert!(paused.can_continue());

        let retryable = fact(
            TaskState::Failed,
            Some(TaskDiagnostic::new(TaskErrorCode::NetworkInterrupted, true)),
            false,
        );
        assert!(retryable.retryable());
        assert!(retryable.can_continue());

        let non_retryable = fact(
            TaskState::Failed,
            Some(TaskDiagnostic::new(TaskErrorCode::PermissionDenied, false)),
            false,
        );
        assert!(!non_retryable.retryable());
        assert!(!non_retryable.can_continue());

        let auto_waiting = fact(TaskState::Interrupted, None, true);
        assert!(auto_waiting.can_pause());
        assert!(!auto_waiting.can_continue());
    }
}
