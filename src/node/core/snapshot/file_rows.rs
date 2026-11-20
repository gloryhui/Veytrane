//! Local file task projection for management clients.

use crate::identity::NodeId;
use crate::node::core::queue::QueueMetrics;
use crate::node::core::task_model::{TaskDiagnostic, TaskDirection, TaskId, TaskRecord, TaskState};
use std::collections::{HashMap, HashSet};
use veytrane_files::task_snapshot::{FileSnapshot, FileTaskSnapshot};

#[derive(Clone, Debug)]
pub struct TaskRow {
    pub id: TaskId,
    pub group: Option<TaskId>,
    pub name: String,
    pub peer: NodeId,
    pub direction: TaskDirection,
    pub state: TaskState,
    pub total: u64,
    pub confirmed: u64,
    pub rate: f64,
    pub diagnostic: Option<&'static str>,
    pub retryable: bool,
    pub auto_waiting: bool,
    pub can_delete_file: bool,
}
impl TaskRow {
    pub fn from_file(task: FileTaskSnapshot) -> Self {
        let retryable = task.diagnostic.is_some_and(TaskDiagnostic::is_retryable);
        let diagnostic = task.diagnostic.map(TaskDiagnostic::safe_message);
        Self {
            id: task.id,
            group: task.group,
            name: task.name,
            peer: task.peer,
            direction: task.direction,
            state: task.state,
            total: task.total,
            confirmed: task.confirmed,
            rate: task.rate,
            diagnostic,
            retryable,
            auto_waiting: task.auto_waiting,
            can_delete_file: task.can_delete_file,
        }
    }

    pub fn from_record(record: &TaskRecord, rate: f64) -> Self {
        Self::from_file(FileTaskSnapshot::from_record(record, rate, false))
    }
    pub fn percent(&self) -> f64 {
        if self.total == 0 {
            if self.state == TaskState::Completed {
                100.
            } else {
                0.
            }
        } else {
            self.confirmed as f64 * 100. / self.total as f64
        }
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
                || (self.state == TaskState::Failed && self.retryable))
    }
    pub fn can_remove_history(&self) -> bool {
        self.state == TaskState::Completed
    }
    pub fn can_delete_file(&self) -> bool {
        self.can_delete_file
    }
    pub fn state_label(&self) -> &'static str {
        if self.auto_waiting {
            "等待自动恢复（重新连接并验证权限）"
        } else {
            state_label(self.state)
        }
    }
}
pub fn state_label(state: TaskState) -> &'static str {
    match state {
        TaskState::Scanning => "扫描中",
        TaskState::Queued => "排队中",
        TaskState::Connecting => "连接中",
        TaskState::Negotiating => "协商中",
        TaskState::Transferring => "传输中",
        TaskState::Pausing => "正在暂停",
        TaskState::Paused => "已暂停",
        TaskState::Finalizing => "正在发布确认",
        TaskState::Completed => "已完成",
        TaskState::Interrupted => "中断待继续",
        TaskState::Failed => "失败",
    }
}
#[derive(Clone, Debug)]
pub struct GroupRow {
    pub id: TaskId,
    pub name: String,
    pub children: usize,
    pub matched: usize,
    pub complete: usize,
    pub failures: usize,
    pub total: u64,
    pub confirmed: u64,
}
impl GroupRow {
    pub fn label(&self) -> String {
        let state = if self.complete == self.children {
            "全部完成"
        } else if self.failures > 0 {
            "部分中断/失败"
        } else {
            "进行中"
        };
        let mut label = format!(
            "{} · {state} · {}/{} 项 · {} / {} 字节",
            self.name, self.complete, self.children, self.confirmed, self.total
        );
        if self.matched < self.children {
            label.push_str(&format!(
                " · 筛选命中 {}/{} 项",
                self.matched, self.children
            ));
        }
        label
    }
}
#[derive(Clone, Debug)]
pub enum ListRow {
    Group(GroupRow),
    Task(TaskRow),
}
pub struct Snapshot {
    pub space: crate::node::core::space_budget::Summary,
    pub tasks: Vec<TaskRow>,
    pub queue: QueueMetrics,
}
impl Snapshot {
    pub fn from_file(file: FileSnapshot) -> Self {
        Self {
            space: file.space,
            tasks: file.tasks.into_iter().map(TaskRow::from_file).collect(),
            queue: file.queue,
        }
    }

    pub fn list(&self, expanded: &HashSet<TaskId>) -> Vec<ListRow> {
        self.filtered_list(expanded, &TaskFilter::default(), &[])
    }
    pub fn filtered_list(
        &self,
        expanded: &HashSet<TaskId>,
        filter: &TaskFilter,
        devices: &[crate::node::core::trusted_devices::TrustedDevice],
    ) -> Vec<ListRow> {
        let mut groups: HashMap<TaskId, GroupRow> = HashMap::new();
        for t in &self.tasks {
            if let Some(id) = &t.group {
                let g = groups.entry(id.clone()).or_insert_with(|| GroupRow {
                    id: id.clone(),
                    name: t.name.split('/').next().unwrap_or(&t.name).to_owned(),
                    children: 0,
                    matched: 0,
                    complete: 0,
                    failures: 0,
                    total: 0,
                    confirmed: 0,
                });
                g.children += 1;
                g.matched += usize::from(filter.matches(t, devices));
                g.complete += usize::from(t.state == TaskState::Completed);
                g.failures += usize::from(matches!(
                    t.state,
                    TaskState::Failed | TaskState::Interrupted
                ));
                g.total = g.total.saturating_add(t.total);
                g.confirmed = g.confirmed.saturating_add(t.confirmed);
            }
        }
        let mut seen = HashSet::new();
        let mut rows = Vec::new();
        for t in &self.tasks {
            if !filter.matches(t, devices) {
                continue;
            }
            if let Some(id) = &t.group {
                if seen.insert(id.clone()) {
                    rows.push(ListRow::Group(groups[id].clone()));
                }
                if !filter.active() && !expanded.contains(id) {
                    continue;
                }
            }
            rows.push(ListRow::Task(t.clone()));
        }
        rows
    }
}

#[derive(Clone, Default)]
pub struct TaskFilter {
    pub query: String,
    pub peer: Option<NodeId>,
    pub direction: Option<TaskDirection>,
    pub state: Option<TaskState>,
}
impl TaskFilter {
    pub fn active(&self) -> bool {
        !self.query.trim().is_empty()
            || self.peer.is_some()
            || self.direction.is_some()
            || self.state.is_some()
    }
    pub fn matches(
        &self,
        row: &TaskRow,
        devices: &[crate::node::core::trusted_devices::TrustedDevice],
    ) -> bool {
        if self.peer.is_some_and(|p| p != row.peer)
            || self.direction.is_some_and(|d| d != row.direction)
            || self.state.is_some_and(|s| s != row.state)
        {
            return false;
        }
        let alias = devices
            .iter()
            .find(|d| d.node_id == row.peer.to_hex())
            .map_or("", |d| d.display_name.as_str());
        let text = format!(
            "{} {} {} {}",
            row.name,
            row.peer.to_hex(),
            row.peer.short(),
            alias
        )
        .to_lowercase();
        self.query
            .to_lowercase()
            .split_whitespace()
            .all(|word| text.contains(word))
    }
}
