//! File-capability commands in the management IPC wire shape.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use veytrane_transport::operation_id::TaskId;

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case", deny_unknown_fields)]
pub enum FileManagementCommand {
    Send { peer: String, paths: Vec<PathBuf> },
    PreviewDirectory { peer: String, path: PathBuf },
    Pause { task: TaskId },
    Resume { peer: String, task: TaskId },
    Cancel { task: TaskId },
    RemoveHistory { tasks: Vec<TaskId> },
    DeleteReceivedFile { task: TaskId },
    TaskLocation { task: TaskId },
    ConfirmDirectory { peer: String, request: TaskId },
    CancelDirectory { request: TaskId },
}

#[cfg(test)]
mod tests {
    use super::FileManagementCommand;
    use serde_json::json;
    use std::path::PathBuf;
    use veytrane_transport::operation_id::TaskId;

    #[test]
    fn file_management_commands_keep_exact_wire_shape() {
        let peer = "11111111111111111111111111111111";
        let task = "22222222222222222222222222222222";
        let request = "33333333333333333333333333333333";
        let task_id = || TaskId::parse(task).unwrap();
        let request_id = || TaskId::parse(request).unwrap();
        let cases = [
            (
                FileManagementCommand::Send {
                    peer: peer.into(),
                    paths: vec![PathBuf::from("/source/a"), PathBuf::from("/source/b")],
                },
                json!({
                    "method": "send",
                    "peer": peer,
                    "paths": ["/source/a", "/source/b"],
                }),
            ),
            (
                FileManagementCommand::PreviewDirectory {
                    peer: peer.into(),
                    path: PathBuf::from("/source"),
                },
                json!({
                    "method": "preview_directory",
                    "peer": peer,
                    "path": "/source",
                }),
            ),
            (
                FileManagementCommand::Pause { task: task_id() },
                json!({"method": "pause", "task": task}),
            ),
            (
                FileManagementCommand::Resume {
                    peer: peer.into(),
                    task: task_id(),
                },
                json!({"method": "resume", "peer": peer, "task": task}),
            ),
            (
                FileManagementCommand::Cancel { task: task_id() },
                json!({"method": "cancel", "task": task}),
            ),
            (
                FileManagementCommand::RemoveHistory {
                    tasks: vec![task_id(), request_id()],
                },
                json!({"method": "remove_history", "tasks": [task, request]}),
            ),
            (
                FileManagementCommand::DeleteReceivedFile { task: task_id() },
                json!({"method": "delete_received_file", "task": task}),
            ),
            (
                FileManagementCommand::TaskLocation { task: task_id() },
                json!({"method": "task_location", "task": task}),
            ),
            (
                FileManagementCommand::ConfirmDirectory {
                    peer: peer.into(),
                    request: request_id(),
                },
                json!({
                    "method": "confirm_directory",
                    "peer": peer,
                    "request": request,
                }),
            ),
            (
                FileManagementCommand::CancelDirectory {
                    request: request_id(),
                },
                json!({"method": "cancel_directory", "request": request}),
            ),
        ];
        for (command, expected) in cases {
            assert_eq!(serde_json::to_value(&command).unwrap(), expected);
            let decoded: FileManagementCommand = serde_json::from_value(expected.clone()).unwrap();
            assert_eq!(serde_json::to_value(decoded).unwrap(), expected);
        }
    }

    #[test]
    fn file_management_commands_reject_unknown_fields() {
        let value = json!({
            "method": "pause",
            "task": "22222222222222222222222222222222",
            "unexpected": true,
        });
        assert!(serde_json::from_value::<FileManagementCommand>(value).is_err());
    }
}
