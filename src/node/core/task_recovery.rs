//! Startup interruption and explicit, TaskId-only recovery lookup.
//!
//! Recovery lookup returns only the already stored local binding. It accepts
//! no remote path and never queues a task by itself.

#[cfg(test)]
use std::path::PathBuf;

use crate::node::core::task_model::{
    TaskDiagnostic, TaskErrorCode, TaskId, TaskModelError, TaskRecord, TaskState,
    system_time_unix_ms,
};

#[cfg(test)]
use crate::node::core::task_model::{ManifestIdentity, PeerId};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct StartupRecoveryReport {
    interrupted_task_ids: Vec<TaskId>,
}

impl StartupRecoveryReport {
    pub(crate) fn interrupted_task_ids(&self) -> &[TaskId] {
        &self.interrupted_task_ids
    }
}

pub(crate) fn recover_startup(
    tasks: &mut [TaskRecord],
) -> Result<StartupRecoveryReport, TaskModelError> {
    let now = system_time_unix_ms()?;
    let mut interrupted_task_ids = Vec::new();

    for task in tasks {
        if task.state().needs_startup_recovery() {
            task.transition_to(
                TaskState::Interrupted,
                Some(TaskDiagnostic::new(
                    TaskErrorCode::ApplicationRestarted,
                    false,
                )),
                now,
            )?;
            interrupted_task_ids.push(task.task_id_owned());
        }
    }

    Ok(StartupRecoveryReport {
        interrupted_task_ids,
    })
}

#[cfg(test)]
mod lookup;
#[cfg(test)]
pub(crate) use lookup::{receiver_recovery, sender_recovery};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::Identity;

    fn record() -> TaskRecord {
        #[cfg(windows)]
        let path = PathBuf::from(r"C:\tmp\local-source.bin");
        #[cfg(not(windows))]
        let path = PathBuf::from("/tmp/local-source.bin");
        TaskRecord::new_sender(
            PeerId::from_node_id(crate::identity::NodeId::from_public_key(
                &Identity::generate().public_key(),
            )),
            path,
            ManifestIdentity::blake3([3; 32], 4, 4).unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn startup_interrupts_active_states_but_preserves_paused_and_terminal_states() {
        let mut active = record();
        let id = active.task_id_owned();
        let report = recover_startup(std::slice::from_mut(&mut active)).unwrap();
        assert_eq!(active.state(), TaskState::Interrupted);
        assert_eq!(
            active.diagnostic().unwrap().code(),
            TaskErrorCode::ApplicationRestarted
        );
        assert_eq!(report.interrupted_task_ids(), &[id]);

        let mut paused = record();
        paused.transition_to(TaskState::Queued, None, 2).unwrap();
        paused
            .transition_to(TaskState::Connecting, None, 3)
            .unwrap();
        paused.transition_to(TaskState::Pausing, None, 4).unwrap();
        paused.transition_to(TaskState::Paused, None, 5).unwrap();
        let mut completed = record();
        completed.transition_to(TaskState::Queued, None, 2).unwrap();
        completed
            .transition_to(TaskState::Connecting, None, 3)
            .unwrap();
        completed
            .transition_to(TaskState::Negotiating, None, 4)
            .unwrap();
        completed
            .transition_to(TaskState::Transferring, None, 5)
            .unwrap();
        completed
            .transition_to(TaskState::Finalizing, None, 6)
            .unwrap();
        completed
            .transition_to(TaskState::Completed, None, 7)
            .unwrap();
        let mut failed = record();
        failed
            .transition_to(
                TaskState::Failed,
                Some(TaskDiagnostic::new(TaskErrorCode::SourceUnavailable, false)),
                8,
            )
            .unwrap();

        let report =
            recover_startup(&mut [paused.clone(), completed.clone(), failed.clone()]).unwrap();
        assert!(report.interrupted_task_ids().is_empty());
    }
}
