use crate::management_command::FileManagementCommand;
use clap::Subcommand;
use std::{io, path::PathBuf};
use veytrane_management::protocol::Command;
use veytrane_transport::operation_id::TaskId;

#[derive(Subcommand)]
pub enum FileAction {
    Tasks {
        #[arg(long, default_value_t = 0)]
        offset: usize,
    },
    Send {
        peer: String,
        #[arg(required = true)]
        paths: Vec<PathBuf>,
    },
    Pause {
        task: String,
    },
    PreviewDirectory {
        peer: String,
        path: PathBuf,
    },
    Resume {
        peer: String,
        task: String,
    },
    Cancel {
        task: String,
    },
    ConfirmDirectory {
        peer: String,
        request: String,
    },
    CancelDirectory {
        request: String,
    },
}

impl FileAction {
    pub fn into_command(self) -> io::Result<Command> {
        let task_id = |value: &str| TaskId::parse(value).map_err(io::Error::other);
        let command = match self {
            Self::Tasks { offset } => return Ok(Command::Tasks { offset }),
            Self::Send { peer, paths } => FileManagementCommand::Send {
                peer,
                paths: paths
                    .into_iter()
                    .map(std::path::absolute)
                    .collect::<io::Result<_>>()?,
            },
            Self::Pause { task } => FileManagementCommand::Pause {
                task: task_id(&task)?,
            },
            Self::PreviewDirectory { peer, path } => FileManagementCommand::PreviewDirectory {
                peer,
                path: std::path::absolute(path)?,
            },
            Self::Resume { peer, task } => FileManagementCommand::Resume {
                peer,
                task: task_id(&task)?,
            },
            Self::Cancel { task } => FileManagementCommand::Cancel {
                task: task_id(&task)?,
            },
            Self::ConfirmDirectory { peer, request } => FileManagementCommand::ConfirmDirectory {
                peer,
                request: task_id(&request)?,
            },
            Self::CancelDirectory { request } => FileManagementCommand::CancelDirectory {
                request: task_id(&request)?,
            },
        };
        Command::file(&command)
    }
}
