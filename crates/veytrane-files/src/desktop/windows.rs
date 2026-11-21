//! File-only secondary windows backed by the shared file workbench model.

use super::*;
use crate::management_command::FileManagementCommand;
use gpui::{
    App, Bounds, Context, Div, FocusHandle, IntoElement, MouseButton, PromptButton, PromptLevel,
    Render, Window, WindowBounds, WindowOptions, div, px, rgb, size,
};
use veytrane_desktop_ui::{components as ui_components, format_bytes, theme as ui_theme};
use veytrane_transport::operation_id::TaskId;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) enum FilePage {
    TaskDetails,
    DirectoryPreview,
}

impl FilePage {
    fn title(self) -> &'static str {
        match self {
            Self::TaskDetails => "Veytrane · 任务详情",
            Self::DirectoryPreview => "Veytrane · 目录预览",
        }
    }

    fn dimensions(self) -> (f32, f32) {
        match self {
            Self::TaskDetails => (900., 480.),
            Self::DirectoryPreview => (1000., 480.),
        }
    }
}

struct FilePanelWindow {
    workbench: Entity<FileWorkbench>,
    page: FilePage,
    focus: FocusHandle,
}

impl Render for FilePanelWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("file-secondary-window-root")
            .size_full()
            .track_focus(&self.focus)
            .child(
                self.workbench
                    .update(cx, |workbench, cx| workbench.panel(self.page, cx)),
            )
    }
}

fn open_panel_window(workbench: Entity<FileWorkbench>, page: FilePage, cx: &mut App) {
    let existing = workbench.read(cx).windows.get(&page).copied();
    if let Some(handle) = existing
        && cx.windows().contains(&handle)
    {
        let _ = handle.update(cx, |_, window, cx| {
            window.activate_window();
            cx.activate(true);
        });
        return;
    }
    let (width, height) = page.dimensions();
    let bounds = Bounds::centered(None, size(px(width), px(height)), cx);
    let panel_workbench = workbench.clone();
    let focus = cx.focus_handle();
    let panel_focus = focus.clone();
    let opened = cx.open_window(
        WindowOptions {
            titlebar: Some(gpui::TitlebarOptions {
                title: Some(page.title().into()),
                ..Default::default()
            }),
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(px(640.), px(280.))),
            ..Default::default()
        },
        move |window, cx| {
            window.focus(&focus);
            window.activate_window();
            cx.new(|cx| {
                cx.observe(&panel_workbench, |_, _, cx| cx.notify())
                    .detach();
                FilePanelWindow {
                    workbench: panel_workbench,
                    page,
                    focus: panel_focus,
                }
            })
        },
    );
    match opened {
        Ok(handle) => {
            workbench.update(cx, |workbench, cx| {
                workbench.windows.insert(page, handle.into());
                cx.notify();
            });
        }
        Err(error) => {
            workbench.update(cx, |workbench, cx| {
                workbench.set_message(format!("打开{}失败：{error}", page.title()).into(), cx);
            });
        }
    }
}

impl FileWorkbench {
    pub(super) fn open_panel(&mut self, page: FilePage, cx: &mut Context<Self>) {
        let workbench = cx.entity();
        // open_window immediately renders its root. Defer the entire operation
        // until the current model and source window borrows have ended.
        cx.defer(move |cx| {
            if workbench.read(cx).active() {
                open_panel_window(workbench, page, cx);
            }
        });
    }

    pub(super) fn close_panels(&mut self, cx: &mut Context<Self>) {
        let windows = std::mem::take(&mut self.windows);
        for handle in windows.into_values() {
            if cx.windows().contains(&handle) {
                let _ = handle.update(cx, |_, window, _| window.remove_window());
            }
        }
        cx.notify();
    }

    fn panel(&mut self, page: FilePage, cx: &mut Context<Self>) -> Stateful<Div> {
        let content = match page {
            FilePage::TaskDetails => self.home_detail(cx),
            FilePage::DirectoryPreview => self.home_preview(cx),
        };
        div()
            .id("file-secondary-window")
            .size_full()
            .overflow_y_scroll()
            .bg(rgb(ui_theme::PAGE_BG))
            .text_color(rgb(ui_theme::TEXT))
            .flex()
            .flex_col()
            .gap_3()
            .p(px(20.))
            .child(content)
            .child(
                div()
                    .flex()
                    .justify_between()
                    .items_center()
                    .gap_3()
                    .child(div().flex_1().text_xs().child(self.message.clone()))
                    .child(
                        ui_components::secondary_button("关闭窗口", true)
                            .on_mouse_up(MouseButton::Left, |_, window, _| window.remove_window()),
                    ),
            )
    }

    fn home_preview(&self, cx: &Context<Self>) -> Div {
        let mut card = ui_components::card().child(ui_components::section_header(
            "▱",
            "目录预览",
            "确认后由后台节点发送变化的文件",
        ));
        for preview in &self.previews {
            let request = preview["id"].as_str().and_then(|id| TaskId::parse(id).ok());
            let peer = preview["peer"].as_str().unwrap_or("").to_owned();
            let cancel = request.clone();
            card = card.child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .text_sm()
                            .child(super::preview_summary(preview, format_bytes)),
                    )
                    .child(
                        ui_components::secondary_button("确认发送", self.available()).on_mouse_up(
                            MouseButton::Left,
                            cx.listener(move |s, _, _, cx| {
                                if let Some(request) = request.clone() {
                                    s.send_file_command(
                                        FileManagementCommand::ConfirmDirectory {
                                            peer: peer.clone(),
                                            request,
                                        },
                                        FileReply::Action,
                                        cx,
                                    );
                                }
                            }),
                        ),
                    )
                    .child(
                        ui_components::secondary_button("取消预览", self.available()).on_mouse_up(
                            MouseButton::Left,
                            cx.listener(move |s, _, _, cx| {
                                if let Some(request) = cancel.clone() {
                                    s.send_file_command(
                                        FileManagementCommand::CancelDirectory { request },
                                        FileReply::Action,
                                        cx,
                                    );
                                }
                            }),
                        ),
                    ),
            );
        }
        card
    }

    fn home_detail(&self, cx: &Context<Self>) -> Div {
        let task = self
            .tasks
            .iter()
            .find(|t| t["id"].as_str() == self.selected.as_deref());
        let mut card = ui_components::card().child(
            div()
                .flex()
                .justify_between()
                .items_center()
                .child(ui_components::section_header(
                    "▤",
                    "任务详情",
                    "状态和已确认字节来自后台持久记录",
                ))
                .child(
                    ui_components::secondary_button("关闭窗口", true).on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|_, _, window, _| window.remove_window()),
                    ),
                ),
        );
        if let Some(task) = task {
            card = card
                .child(task["name"].as_str().unwrap_or("").to_owned())
                .child(format!(
                    "设备：{} · {}",
                    task["peer"].as_str().unwrap_or(""),
                    task_state(task)
                ));
            if let Some(diagnostic) = task["diagnostic"].as_str() {
                card = card.child(diagnostic.to_owned());
            }
            let id = task["id"].as_str().and_then(|id| TaskId::parse(id).ok());
            let delete = id.clone();
            let activation = self.activation;
            card = card.child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        ui_components::secondary_button("定位文件", self.available()).on_mouse_up(
                            MouseButton::Left,
                            cx.listener(move |s, _, _, cx| {
                                if let Some(task) = id.clone() {
                                    s.send_file_command(
                                        FileManagementCommand::TaskLocation { task },
                                        FileReply::Location,
                                        cx,
                                    );
                                }
                            }),
                        ),
                    )
                    .when(task["can_delete_file"].as_bool() == Some(true), |d| {
                        d.child(
                            ui_components::secondary_button("删除接收文件", self.available())
                                .on_mouse_up(
                                    MouseButton::Left,
                                    cx.listener(move |_s, _, window, cx| {
                                        let Some(task) = delete.clone() else {
                                            return;
                                        };
                                        let answer = window.prompt(
                                            PromptLevel::Warning,
                                            "永久删除接收文件？",
                                            Some("此操作会删除本机文件及任务记录，无法撤销。"),
                                            &[
                                                PromptButton::Ok("删除".into()),
                                                PromptButton::Cancel("取消".into()),
                                            ],
                                            cx,
                                        );
                                        cx.spawn(async move |workbench, cx| {
                                            if answer.await == Ok(0) {
                                                let _ = workbench.update(cx, |workbench, cx| {
                                                    if workbench.activation == activation {
                                                        workbench.send_file_command(
                                                        FileManagementCommand::DeleteReceivedFile {
                                                            task,
                                                        },
                                                        FileReply::Action,
                                                        cx,
                                                    );
                                                    }
                                                });
                                            }
                                        })
                                        .detach();
                                    }),
                                ),
                        )
                    }),
            );
        } else {
            card = card.child("任务已移除或位于其它页。");
        }
        card
    }
}
