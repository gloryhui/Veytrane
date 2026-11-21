//! File workspace and tools hosted by the desktop over management IPC.

use crate::management_command::FileManagementCommand;
use gpui::{
    AnyWindowHandle, App, Context, Div, Entity, EventEmitter, ExternalPaths, KeyBinding,
    MouseButton, SharedString, Stateful, div, prelude::*,
};
use serde_json::{Value, json};
use std::collections::HashMap;
use veytrane_desktop_ui::{
    components as ui_components, format_bytes, text_field::TextField, theme as ui_theme,
};
use veytrane_management::protocol::Command;

mod views;
mod windows;
use windows::FilePage;

gpui::actions!(agent_shell, [ChooseFiles, ChooseFolder]);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FileSupport {
    #[default]
    Unavailable,
    Disabled,
    Active,
}

impl FileSupport {
    pub fn from_status(state: &Value) -> Self {
        if state["core_running"] != true
            || state["restart_pending"] == true
            || state["files_compiled"] == false
        {
            Self::Unavailable
        } else if state["capabilities"]["files"] == true {
            Self::Active
        } else if state["files_compiled"] == true {
            Self::Disabled
        } else {
            Self::Unavailable
        }
    }

    pub fn peer_supports_files(peer: &Value) -> bool {
        peer["negotiated_capabilities"]
            .as_u64()
            .is_none_or(|bits| bits & veytrane_node::capability::CAP_FILE_TRANSFER != 0)
    }
}

#[derive(Default)]
pub struct FileHost {
    pub support: FileSupport,
    pub available: bool,
    pub peer: Option<String>,
    pub peer_can_send: bool,
}

#[derive(Clone, Copy)]
pub enum FileReply {
    Action,
    Tasks,
    Location,
}

pub enum FileEvent {
    Request(Command, FileReply),
    Setting {
        key: &'static str,
        value: Value,
        restart: bool,
    },
    Resource {
        key: &'static str,
        allowed: bool,
    },
    Message(SharedString),
}

pub struct FileWorkbench {
    host: FileHost,
    settings: Option<Value>,
    task_page: Value,
    tasks: Vec<Value>,
    previews: Vec<Value>,
    search: Entity<TextField>,
    upload: Entity<TextField>,
    download: Entity<TextField>,
    margin: Entity<TextField>,
    direction: u8,
    state_filter: u8,
    peer_filter: bool,
    selected: Option<String>,
    windows: HashMap<FilePage, AnyWindowHandle>,
    message: SharedString,
    activation: u64,
}

impl EventEmitter<FileEvent> for FileWorkbench {}

impl FileWorkbench {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            host: FileHost::default(),
            settings: None,
            task_page: Value::Null,
            tasks: Vec::new(),
            previews: Vec::new(),
            search: cx.new(|cx| TextField::new(cx, "搜索文件名或设备备注 / ID")),
            upload: cx.new(|cx| TextField::new(cx, "0")),
            download: cx.new(|cx| TextField::new(cx, "0")),
            margin: cx.new(|cx| TextField::new(cx, "64")),
            direction: 0,
            state_filter: 0,
            peer_filter: false,
            selected: None,
            windows: HashMap::new(),
            message: "".into(),
            activation: 0,
        }
    }

    pub fn bind_keys(cx: &mut App) {
        cx.bind_keys([
            KeyBinding::new("secondary-o", ChooseFiles, Some("AgentWorkbench")),
            KeyBinding::new("secondary-shift-o", ChooseFolder, Some("AgentWorkbench")),
        ]);
    }

    pub fn sync_host(&mut self, host: FileHost, message: &SharedString, cx: &mut Context<Self>) {
        let was_active = self.active();
        let message_changed = self.message != *message;
        let available_changed = self.host.available != host.available;
        self.host = host;
        self.message = message.clone();
        if was_active && !self.active() {
            self.activation = self.activation.wrapping_add(1);
            self.task_page = Value::Null;
            self.tasks.clear();
            self.previews.clear();
            self.selected = None;
            self.direction = 0;
            self.state_filter = 0;
            self.peer_filter = false;
            set_text(&self.search, String::new(), cx);
            self.close_panels(cx);
        }
        if was_active != self.active() || message_changed || available_changed {
            cx.notify();
        }
    }

    pub fn load_settings(&mut self, settings: &Value, cx: &mut Context<Self>) {
        self.settings = Some(json!({
            "enable_files": settings["enable_files"],
            "receive_root": settings["receive_root"],
            "send_concurrency": settings["send_concurrency"],
            "auto_resume": settings["auto_resume"],
            "receive_safety_mib": settings["receive_safety_mib"],
            "file_limits": settings["file_limits"],
            "trusted_devices": settings["trusted_devices"],
        }));
        for (input, value) in [
            (
                &self.upload,
                settings["file_limits"]["upload_kib"].as_u64().unwrap_or(0),
            ),
            (
                &self.download,
                settings["file_limits"]["download_kib"]
                    .as_u64()
                    .unwrap_or(0),
            ),
            (
                &self.margin,
                settings["receive_safety_mib"].as_u64().unwrap_or(64),
            ),
        ] {
            set_text(input, value.to_string(), cx);
        }
        cx.notify();
    }

    pub fn replace_task_page(&mut self, value: Value, cx: &mut Context<Self>) {
        if !self.active() {
            return;
        }
        let tasks = value["tasks"].as_array().cloned().unwrap_or_default();
        let previews = value["directory_previews"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let new_preview = previews.iter().any(|preview| {
            preview["id"].as_str().is_some()
                && !self
                    .previews
                    .iter()
                    .any(|previous| previous["id"] == preview["id"])
        });
        self.tasks = tasks;
        self.previews = previews;
        self.task_page = value;
        if new_preview {
            self.open_panel(FilePage::DirectoryPreview, cx);
        }
        cx.notify();
    }

    pub fn task_poll(&self) -> Option<Command> {
        self.active().then(|| Command::Tasks {
            offset: TaskPagination::from_value(&self.task_page).offset,
        })
    }

    pub fn legacy_speed_rows(&self) -> Option<&[Value]> {
        self.task_page["speeds"].as_array().map(Vec::as_slice)
    }

    pub fn tray_activity(&self) -> Option<String> {
        let active = self.task_page["queue"]["active_files"]
            .as_u64()
            .unwrap_or(0);
        (self.active() && active > 0).then(|| format!("{active} 个传输"))
    }

    pub fn search_field(&self) -> Option<&Entity<TextField>> {
        self.active().then_some(&self.search)
    }

    pub fn settings_fields(&self) -> impl Iterator<Item = &Entity<TextField>> {
        [&self.upload, &self.download, &self.margin]
            .into_iter()
            .filter(|_| self.active())
    }

    pub fn mount_home(&mut self, root: Stateful<Div>, cx: &mut Context<Self>) -> Stateful<Div> {
        if !self.active() {
            return root;
        }
        let drop_enabled = self.can_send_files();
        let receive = self.home_receive(cx);
        let tasks = self.home_tasks(cx);
        root.can_drop(move |value, _, _| {
            drop_enabled
                && value
                    .downcast_ref::<ExternalPaths>()
                    .is_some_and(|paths| crate::path_batch::validate(paths.paths()).is_ok())
        })
        .on_drop(cx.listener(|files, paths: &ExternalPaths, _, cx| {
            if files.can_send_files()
                && let Some(peer) = files.host.peer.clone()
            {
                files.send_file_command(
                    FileManagementCommand::Send {
                        peer,
                        paths: paths.paths().to_vec(),
                    },
                    FileReply::Action,
                    cx,
                );
            }
        }))
        .on_action(cx.listener(|files, _: &ChooseFiles, _, cx| files.home_pick(false, false, cx)))
        .on_action(cx.listener(|files, _: &ChooseFolder, _, cx| files.home_pick(true, false, cx)))
        .child(receive)
        .child(tasks)
        .when(!self.previews.is_empty(), |root| {
            root.child(
                ui_components::secondary_button("查看待确认的目录预览", true).on_mouse_up(
                    MouseButton::Left,
                    cx.listener(|files, _, _, cx| {
                        files.open_panel(FilePage::DirectoryPreview, cx);
                    }),
                ),
            )
        })
    }

    pub fn resource_controls(&self, cx: &Context<Self>) -> Option<Div> {
        self.active().then(|| {
            div()
                .flex()
                .gap_2()
                .children([("允许文件访问", true), ("撤销文件访问", false)].map(
                    |(label, allowed)| {
                        ui_components::primary_button(label, self.available()).on_click(
                            cx.listener(move |files, _, _, cx| {
                                if files.available() {
                                    cx.emit(FileEvent::Resource {
                                        key: "files",
                                        allowed,
                                    });
                                }
                            }),
                        )
                    },
                ))
        })
    }

    fn active(&self) -> bool {
        self.host.support == FileSupport::Active
    }

    fn available(&self) -> bool {
        self.active() && self.host.available
    }

    fn can_send_files(&self) -> bool {
        self.available() && self.host.peer_can_send && self.host.peer.is_some()
    }

    fn set_message(&mut self, message: SharedString, cx: &mut Context<Self>) {
        self.message = message.clone();
        cx.emit(FileEvent::Message(message));
        cx.notify();
    }

    fn send_file_command(
        &mut self,
        command: FileManagementCommand,
        reply: FileReply,
        cx: &mut Context<Self>,
    ) {
        if !self.available() {
            return;
        }
        match Command::file(&command) {
            Ok(command) => cx.emit(FileEvent::Request(command, reply)),
            Err(error) => self.set_message(format!("文件命令无法序列化：{error}").into(), cx),
        }
    }

    fn request_tasks(&self, offset: usize, cx: &mut Context<Self>) {
        if self.available() {
            cx.emit(FileEvent::Request(
                Command::Tasks { offset },
                FileReply::Tasks,
            ));
        }
    }

    fn update_setting(&self, key: &'static str, value: Value, cx: &mut Context<Self>) {
        if self.available() {
            cx.emit(FileEvent::Setting {
                key,
                value,
                restart: false,
            });
        }
    }

    fn text(field: &Entity<TextField>, cx: &Context<Self>) -> String {
        field.read(cx).content.trim().to_owned()
    }
}

fn set_text(input: &Entity<TextField>, text: String, cx: &mut Context<FileWorkbench>) {
    input.update(cx, |field, cx| {
        field.content = text.into();
        field.selected_range = field.content.len()..field.content.len();
        cx.notify();
    });
}

/// Task page size used by the desktop list projection.
pub const TASK_PAGE_SIZE: usize = 128;

pub fn task_state(task: &Value) -> &'static str {
    match task["state"].as_str().unwrap_or("") {
        "scanning" => "扫描中",
        "queued" => "排队中",
        "connecting" => "连接中",
        "negotiating" => "协商中",
        "transferring" => "传输中",
        "pausing" => "正在暂停",
        "paused" => "已暂停",
        "finalizing" => "正在发布确认",
        "completed" => "已完成",
        "interrupted" => "中断待继续",
        "failed" => "失败",
        _ => "未知",
    }
}

pub fn matches_task(
    task: &Value,
    query: &str,
    direction: u8,
    state: u8,
    peer: Option<&str>,
    devices: &Value,
) -> bool {
    let task_peer = task["peer"].as_str().unwrap_or("");
    if peer.is_some_and(|peer| peer != task_peer) {
        return false;
    }
    if direction != 0
        && task["direction"].as_str() != Some(if direction == 1 { "send" } else { "receive" })
    {
        return false;
    }
    let phase = task["state"].as_str().unwrap_or("");
    if match state {
        1 => !matches!(
            phase,
            "scanning"
                | "queued"
                | "connecting"
                | "negotiating"
                | "transferring"
                | "pausing"
                | "finalizing"
        ),
        2 => !matches!(phase, "paused" | "interrupted"),
        3 => phase != "completed",
        4 => phase != "failed",
        _ => false,
    } {
        return false;
    }
    let query = query.trim().to_lowercase();
    query.is_empty()
        || task["name"]
            .as_str()
            .unwrap_or("")
            .to_lowercase()
            .contains(&query)
        || task_peer.contains(&query)
        || devices.as_array().into_iter().flatten().any(|d| {
            d["node_id"].as_str() == Some(task_peer)
                && d["display_name"]
                    .as_str()
                    .unwrap_or("")
                    .to_lowercase()
                    .contains(&query)
        })
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TaskPagination {
    pub total: u64,
    pub offset: usize,
    pub next_offset: Option<usize>,
}

impl TaskPagination {
    pub fn from_value(task_page: &Value) -> Self {
        Self {
            total: task_page["total"].as_u64().unwrap_or(0),
            offset: task_page["offset"].as_u64().unwrap_or(0) as usize,
            next_offset: task_page["next_offset"].as_u64().map(|n| n as usize),
        }
    }

    pub fn previous_offset(&self) -> usize {
        self.offset.saturating_sub(TASK_PAGE_SIZE)
    }
}

pub fn preview_summary(preview: &Value, format_bytes: impl FnOnce(f64) -> String) -> String {
    format!(
        "{} · 新增 {} · 变化 {} · 不变 {} · 待传 {}",
        preview["name"].as_str().unwrap_or("目录"),
        preview["added"],
        preview["changed"],
        preview["unchanged"],
        format_bytes(preview["transfer_bytes"].as_u64().unwrap_or(0) as f64)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effective_agent_file_support_controls_enablement_and_mounting() {
        let support = |compiled, active, restart_pending| {
            FileSupport::from_status(&json!({
                "core_running": true,
                "files_compiled": compiled,
                "capabilities": { "files": active },
                "restart_pending": restart_pending,
            }))
        };
        assert_eq!(support(false, false, false), FileSupport::Unavailable);
        assert_eq!(support(false, true, false), FileSupport::Unavailable);
        assert_eq!(support(true, false, false), FileSupport::Disabled);
        assert_eq!(support(true, true, false), FileSupport::Active);
        assert_eq!(support(true, true, true), FileSupport::Unavailable);
        assert_eq!(
            FileSupport::from_status(&Value::Null),
            FileSupport::Unavailable
        );
        assert_eq!(
            FileSupport::from_status(&json!({"core_running":true,"capabilities":{"files":true}})),
            FileSupport::Active
        );
        assert!(!FileSupport::peer_supports_files(&json!({
            "negotiated_capabilities": veytrane_node::capability::CAP_TCP_TUNNEL,
        })));
        assert!(FileSupport::peer_supports_files(&json!({
            "negotiated_capabilities": veytrane_node::capability::CAP_FILE_TRANSFER,
        })));
        assert!(FileSupport::peer_supports_files(&json!({})));
    }
    use serde_json::json;

    #[test]
    fn filters_keep_direction_state_and_device_identity_distinct() {
        let task =
            json!({"name":"report.txt","peer":"abc","direction":"receive","state":"completed"});
        let devices = json!([{"node_id":"abc","display_name":"Home PC"}]);
        assert!(matches_task(&task, "home", 2, 3, Some("abc"), &devices));
        assert!(!matches_task(&task, "home", 1, 3, Some("abc"), &devices));
        assert!(!matches_task(&task, "home", 2, 1, Some("abc"), &devices));
        assert!(!matches_task(&task, "home", 2, 3, Some("other"), &devices));
    }

    #[test]
    fn task_state_covers_wire_states_and_falls_back_to_unknown() {
        let cases = [
            ("scanning", "扫描中"),
            ("queued", "排队中"),
            ("connecting", "连接中"),
            ("negotiating", "协商中"),
            ("transferring", "传输中"),
            ("pausing", "正在暂停"),
            ("paused", "已暂停"),
            ("finalizing", "正在发布确认"),
            ("completed", "已完成"),
            ("interrupted", "中断待继续"),
            ("failed", "失败"),
            ("new-state", "未知"),
        ];
        for (state, expected) in cases {
            assert_eq!(task_state(&json!({ "state": state })), expected);
        }
        assert_eq!(task_state(&json!({})), "未知");
    }

    #[test]
    fn task_pagination_projects_offsets_and_previous_page_floor() {
        assert_eq!(
            TaskPagination::from_value(&Value::Null),
            TaskPagination::default()
        );
        let page = TaskPagination::from_value(&json!({
            "total": 300,
            "offset": 256,
            "next_offset": 384
        }));
        assert_eq!(
            page,
            TaskPagination {
                total: 300,
                offset: 256,
                next_offset: Some(384)
            }
        );
        assert_eq!(page.previous_offset(), 128);
        assert_eq!(
            TaskPagination {
                total: 0,
                offset: 64,
                next_offset: None
            }
            .previous_offset(),
            0
        );
    }

    #[test]
    fn preview_summary_keeps_field_order_and_name_fallback() {
        assert_eq!(
            preview_summary(
                &json!({
                    "name": "photos",
                    "added": 2,
                    "changed": 3,
                    "unchanged": 4,
                    "transfer_bytes": 1536
                }),
                |value| format!("{value:.0} B")
            ),
            "photos · 新增 2 · 变化 3 · 不变 4 · 待传 1536 B"
        );
        assert_eq!(
            preview_summary(&json!({}), |value| format!("{value:.0} B")),
            "目录 · 新增 null · 变化 null · 不变 null · 待传 0 B"
        );
    }
}
