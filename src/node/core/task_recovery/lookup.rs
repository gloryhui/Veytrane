//! Historical explicit-lookup regression fixtures, not part of the runtime.
use super::*;
use crate::node::core::{
    task_model::TaskDirection,
    task_store::{TaskStore, TaskStoreError},
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct SenderRecovery {
    task_id: TaskId,
    peer_id: PeerId,
    source_path: PathBuf,
    manifest_identity: ManifestIdentity,
    state: TaskState,
}

impl SenderRecovery {
    pub(crate) fn task_id(&self) -> &TaskId {
        &self.task_id
    }
    #[allow(dead_code)] // Consumed by the later peer-session task adapter.
    pub(crate) fn peer_id(&self) -> &PeerId {
        &self.peer_id
    }
    pub(crate) fn source_path(&self) -> &std::path::Path {
        &self.source_path
    }
    #[allow(dead_code)] // The sender executor binds this manifest in the later task.
    pub(crate) fn manifest_identity(&self) -> &ManifestIdentity {
        &self.manifest_identity
    }
    pub(crate) fn state(&self) -> TaskState {
        self.state
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ReceiverRecovery {
    task_id: TaskId,
    peer_id: PeerId,
    receive_root: PathBuf,
    manifest_identity: ManifestIdentity,
    state: TaskState,
}

impl ReceiverRecovery {
    pub(crate) fn task_id(&self) -> &TaskId {
        &self.task_id
    }
    pub(crate) fn peer_id(&self) -> &PeerId {
        &self.peer_id
    }
    pub(crate) fn receive_root(&self) -> &std::path::Path {
        &self.receive_root
    }
    #[allow(dead_code)] // The receiver executor binds this manifest in the later task.
    pub(crate) fn manifest_identity(&self) -> &ManifestIdentity {
        &self.manifest_identity
    }
    pub(crate) fn state(&self) -> TaskState {
        self.state
    }
}

pub(crate) fn sender_recovery(
    store: &TaskStore,
    task_id: &TaskId,
) -> Result<SenderRecovery, TaskStoreError> {
    let task = store.task(task_id)?;
    if task.direction() != TaskDirection::Send {
        return Err(TaskStoreError::Model(TaskModelError::WrongDirection));
    }
    ensure_explicitly_recoverable(&task)?;
    Ok(SenderRecovery {
        task_id: task.task_id_owned(),
        peer_id: task.peer_id().clone(),
        source_path: task.local_path().to_path_buf(),
        manifest_identity: task.manifest_identity().clone(),
        state: task.state(),
    })
}

pub(crate) fn receiver_recovery(
    store: &TaskStore,
    task_id: &TaskId,
) -> Result<ReceiverRecovery, TaskStoreError> {
    let task = store.task(task_id)?;
    if task.direction() != TaskDirection::Receive {
        return Err(TaskStoreError::Model(TaskModelError::WrongDirection));
    }
    ensure_explicitly_recoverable(&task)?;
    Ok(ReceiverRecovery {
        task_id: task.task_id_owned(),
        peer_id: task.peer_id().clone(),
        receive_root: task.local_path().to_path_buf(),
        manifest_identity: task.manifest_identity().clone(),
        state: task.state(),
    })
}

fn ensure_explicitly_recoverable(task: &TaskRecord) -> Result<(), TaskStoreError> {
    let state_is_recoverable = matches!(task.state(), TaskState::Paused | TaskState::Interrupted)
        || (task.state() == TaskState::Failed
            && task.diagnostic().is_some_and(TaskDiagnostic::is_retryable));

    if state_is_recoverable {
        Ok(())
    } else {
        Err(TaskStoreError::Model(TaskModelError::NotRecoverable))
    }
}
