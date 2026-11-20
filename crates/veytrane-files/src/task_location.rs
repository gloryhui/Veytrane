//! Resolves and revalidates local paths from durable file-task bindings.

use crate::task_model::{TaskDirection, TaskId, TaskRecord, TaskState};
use std::path::PathBuf;
use veytrane_transport::error::Result;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Selection {
    Task(TaskId),
    Group(TaskId),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocationKind {
    ReceiveRoot,
    SourceDirectory,
    SourceFile,
    ReceivedFile,
    ReceivedDirectory,
}

pub fn records_for<'a>(records: &'a [TaskRecord], selected: &Selection) -> Vec<&'a TaskRecord> {
    match selected {
        Selection::Task(id) => records
            .iter()
            .filter(|record| record.task_id() == id)
            .collect(),
        Selection::Group(id) => records
            .iter()
            .filter(|record| record.group_id() == Some(id))
            .collect(),
    }
}

pub fn anchor<'a>(records: &[&'a TaskRecord]) -> Option<&'a TaskRecord> {
    records.iter().copied().min_by_key(|record| {
        (
            record.directory_details().is_none(),
            record
                .relative_path()
                .map_or(usize::MAX, |path| path.split('/').count()),
            record.task_id().clone(),
        )
    })
}

pub fn location(records: &[&TaskRecord], selection: &Selection) -> Result<(PathBuf, LocationKind)> {
    let record = anchor(records)
        .ok_or_else(|| crate::transfer_files::failure("任务或目录组记录已不存在"))?;
    if matches!(selection, Selection::Group(_)) && record.direction() == TaskDirection::Receive {
        return Ok((record.local_path().to_path_buf(), LocationKind::ReceiveRoot));
    }
    if matches!(selection, Selection::Group(_))
        && record.file_details().is_some()
        && record.direction() == TaskDirection::Send
    {
        let root = record
            .file_details()
            .unwrap()
            .source_root
            .as_ref()
            .ok_or_else(|| crate::transfer_files::failure("目录源绑定不可用"))?;
        return Ok((root.clone(), LocationKind::SourceDirectory));
    }
    if record.direction() == TaskDirection::Send {
        Ok((
            record.local_path().to_path_buf(),
            if record.directory_details().is_none() {
                LocationKind::SourceFile
            } else {
                LocationKind::SourceDirectory
            },
        ))
    } else if records
        .iter()
        .all(|record| record.state() == TaskState::Completed && record.receipt_committed())
    {
        let relative = record
            .relative_path()
            .ok_or_else(|| crate::transfer_files::failure("任务缺少相对路径"))?;
        crate::protocol::validate_relative_path(relative)?;
        Ok((
            record.local_path().join(relative),
            if record.file_details().is_some() {
                LocationKind::ReceivedFile
            } else {
                LocationKind::ReceivedDirectory
            },
        ))
    } else {
        Ok((record.local_path().to_path_buf(), LocationKind::ReceiveRoot))
    }
}

/// Revalidate the stored task's own no-follow location.
pub fn reveal(records: &[TaskRecord], selection: &Selection) -> Result<PathBuf> {
    let selected = records_for(records, selection);
    let first = selected
        .first()
        .ok_or_else(|| crate::transfer_files::failure("任务或目录组记录已不存在"))?;
    if selected.iter().any(|record| {
        record.peer_id() != first.peer_id()
            || record.direction() != first.direction()
            || (first.direction() == TaskDirection::Receive
                && record.local_path() != first.local_path())
    }) {
        return Err(crate::transfer_files::failure("任务组绑定不一致"));
    }
    let record = anchor(&selected)
        .ok_or_else(|| crate::transfer_files::failure("任务或目录组记录已不存在"))?;
    let (path, _) = location(&selected, selection)?;
    if record.direction() == TaskDirection::Send {
        if matches!(selection, Selection::Group(_)) && record.file_details().is_some() {
            let _ = crate::secure_fs::root(&path)?;
        } else if record.directory_details().is_none() {
            let (parent, name) = if record
                .file_details()
                .is_some_and(|details| details.source_root.is_some())
            {
                crate::transfer_files::source_parent(record)?
            } else {
                let path = record
                    .local_path()
                    .parent()
                    .ok_or_else(|| crate::transfer_files::failure("源目录不可用"))?;
                let parent = if path.parent().is_some() {
                    crate::secure_fs::root(path)?
                } else {
                    cap_std::fs::Dir::open_ambient_dir(path, cap_std::ambient_authority())?
                };
                let name = record
                    .local_path()
                    .file_name()
                    .and_then(|name| name.to_str())
                    .ok_or_else(|| crate::transfer_files::failure("源文件名不可用"))?;
                (parent, name.to_owned())
            };
            let _ = crate::secure_fs::open(&parent, &name, false, false)?;
        } else {
            let group_root = record.group_id().and_then(|id| {
                records
                    .iter()
                    .filter(|candidate| {
                        candidate.group_id() == Some(id)
                            && candidate.direction() == TaskDirection::Send
                            && candidate.peer_id() == record.peer_id()
                            && candidate.directory_details().is_some()
                    })
                    .min_by_key(|candidate| candidate.relative_path().unwrap().split('/').count())
            });
            let root_record = group_root.unwrap_or(record);
            let root = crate::secure_fs::root(root_record.local_path())?;
            if root_record.task_id() != record.task_id() {
                let relative = record
                    .relative_path()
                    .unwrap()
                    .split_once('/')
                    .map(|(_, path)| path)
                    .ok_or_else(|| crate::transfer_files::failure("源目录绑定无效"))?;
                if root_record.local_path().join(relative) != record.local_path() {
                    return Err(crate::transfer_files::failure("源目录绑定无效"));
                }
                let (parent, name) = crate::secure_fs::parent(&root, relative, false)?;
                let _ = crate::secure_fs::child(&parent, &name, false)?;
            }
        }
    } else {
        let root = crate::secure_fs::root(record.local_path())?;
        if path != record.local_path() {
            let (parent, name) =
                crate::secure_fs::parent(&root, record.relative_path().unwrap(), false)?;
            if record.file_details().is_some() {
                let _ = crate::secure_fs::open(&parent, &name, false, false)?;
            } else {
                let _ = crate::secure_fs::child(&parent, &name, false)?;
            }
        }
    }
    Ok(path)
}
