use super::*;
use gpui::{CursorStyle, PathPromptOptions, px, relative, rgb};
use std::ops::Range;
use ui_components::input_frame;
use veytrane_transport::operation_id::TaskId;

impl FileWorkbench {
    pub(super) fn home_pick(&mut self, directory: bool, incremental: bool, cx: &mut Context<Self>) {
        if !self.can_send_files() {
            return;
        }
        let Some(peer) = self.host.peer.clone() else {
            return;
        };
        let activation = self.activation;
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: !directory,
            directories: directory,
            multiple: !directory,
            prompt: Some(
                if directory {
                    "选择发送目录"
                } else {
                    "选择发送文件"
                }
                .into(),
            ),
        });
        cx.spawn(async move |shell, cx| match picked.await {
            Ok(Ok(Some(paths))) => {
                let _ = shell.update(cx, |s, cx| {
                    if s.activation != activation {
                        return;
                    }
                    if !s.can_send_files() || s.host.peer.as_ref() != Some(&peer) {
                        s.set_message("连接或接收设备已变化，请重新选择。".into(), cx);
                        return;
                    }
                    let command = if incremental {
                        let Some(path) = paths.into_iter().next() else {
                            return;
                        };
                        FileManagementCommand::PreviewDirectory { peer, path }
                    } else {
                        FileManagementCommand::Send { peer, paths }
                    };
                    s.send_file_command(command, FileReply::Action, cx);
                });
            }
            Ok(Ok(None)) => (),
            _ => {
                let _ = shell.update(cx, |s, cx| {
                    if s.activation == activation {
                        s.set_message("无法打开文件选择器。".into(), cx);
                    }
                });
            }
        })
        .detach();
    }

    fn pick_agent_receive_root(&mut self, cx: &mut Context<Self>) {
        if !self.available() {
            return;
        }
        let activation = self.activation;
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("选择接收目录".into()),
        });
        cx.spawn(async move |shell, cx| {
            if let Ok(Ok(Some(paths))) = picked.await
                && let Some(path) = paths.into_iter().next()
            {
                let _ = shell.update(cx, |s, cx| {
                    if s.activation == activation {
                        s.update_setting("receive_root", json!(path), cx);
                    }
                });
            }
        })
        .detach();
    }

    pub(super) fn home_receive(&self, cx: &Context<Self>) -> gpui::Div {
        let directory = self
            .settings
            .as_ref()
            .and_then(|d| d["receive_root"].as_str())
            .unwrap_or("等待后台节点配置");
        ui_components::card()
            .h(px(184.))
            .p(px(12.))
            .gap_2()
            .child(ui_components::section_header(
                "▱",
                "接收目录",
                "接收文件默认保存在此处；目录更改仅新任务生效（仅本机可见）",
            ))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .h(px(36.))
                            .flex()
                            .items_center()
                            .gap_2()
                            .px(px(10.))
                            .rounded_md()
                            .border_1()
                            .border_color(rgb(ui_theme::BORDER))
                            .bg(rgb(ui_theme::SURFACE_SUBTLE))
                            .child(div().text_color(rgb(ui_theme::PRIMARY)).child("▱"))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_sm()
                                    .text_color(rgb(ui_theme::TEXT_SECONDARY))
                                    .child(directory.to_owned()),
                            ),
                    )
                    .child(
                        ui_components::secondary_button(
                            "更改",
                            self.available() && self.settings.is_some(),
                        )
                        .on_mouse_up(
                            MouseButton::Left,
                            cx.listener(|s, _, _, cx| s.pick_agent_receive_root(cx)),
                        ),
                    ),
            )
    }

    pub(super) fn home_tasks(&mut self, cx: &mut Context<Self>) -> gpui::Div {
        let enabled = self.can_send_files();
        let query = Self::text(&self.search, cx);
        let selected_peer = self.host.peer.clone();
        let devices = self
            .settings
            .as_ref()
            .map(|d| &d["trusted_devices"])
            .unwrap_or(&Value::Null);
        let filtered: Vec<_> = self
            .tasks
            .iter()
            .filter(|t| {
                matches_task(
                    t,
                    &query,
                    self.direction,
                    self.state_filter,
                    if self.peer_filter {
                        Some(selected_peer.as_deref().unwrap_or(""))
                    } else {
                        None
                    },
                    devices,
                )
            })
            .cloned()
            .collect();
        let pagination = TaskPagination::from_value(&self.task_page);
        let total = pagination.total;
        let offset = pagination.offset;
        let next = pagination.next_offset;
        let previous = pagination.previous_offset();
        let mut toolbar = div()
            .flex()
            .items_center()
            .justify_between()
            .gap_3()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(
                                ui_components::compact_secondary_button("选择文件", enabled)
                                    .on_mouse_up(
                                        MouseButton::Left,
                                        cx.listener(|s, _, _, cx| s.home_pick(false, false, cx)),
                                    ),
                            )
                            .child(
                                ui_components::compact_secondary_button("选择目录", enabled)
                                    .on_mouse_up(
                                        MouseButton::Left,
                                        cx.listener(|s, _, _, cx| s.home_pick(true, false, cx)),
                                    ),
                            )
                            .child(
                                ui_components::compact_secondary_button("增量发送目录", enabled)
                                    .on_mouse_up(
                                        MouseButton::Left,
                                        cx.listener(|s, _, _, cx| s.home_pick(true, true, cx)),
                                    ),
                            ),
                    )
                    .child(div().text_xs().text_color(rgb(ui_theme::TEXT_MUTED)).child(
                        if enabled {
                            "可拖入文件和目录 · 每批最多 128 项"
                        } else {
                            "连接并授权后可拖入文件和目录"
                        },
                    )),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_xs()
                    .child("并发：")
                    .child(
                        ui_components::compact_secondary_button(
                            self.settings
                                .as_ref()
                                .and_then(|d| d["send_concurrency"].as_u64())
                                .unwrap_or(1)
                                .to_string(),
                            self.available(),
                        )
                        .on_mouse_up(
                            MouseButton::Left,
                            cx.listener(|s, _, _, cx| {
                                let current = s
                                    .settings
                                    .as_ref()
                                    .and_then(|d| d["send_concurrency"].as_u64())
                                    .unwrap_or(1);
                                s.update_setting(
                                    "send_concurrency",
                                    json!(if current >= 3 { 1 } else { current + 1 }),
                                    cx,
                                );
                            }),
                        ),
                    )
                    .child(format!("任务 {}/{total}", filtered.len())),
            );
        if offset > 0 || next.is_some() {
            toolbar = toolbar.child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        ui_components::compact_secondary_button(
                            "上一页",
                            self.available() && offset > 0,
                        )
                        .on_mouse_up(
                            MouseButton::Left,
                            cx.listener(move |s, _, _, cx| {
                                if offset > 0 {
                                    s.request_tasks(previous, cx);
                                }
                            }),
                        ),
                    )
                    .child(
                        ui_components::compact_secondary_button(
                            "下一页",
                            self.available() && next.is_some(),
                        )
                        .on_mouse_up(
                            MouseButton::Left,
                            cx.listener(move |s, _, _, cx| {
                                if let Some(offset) = next {
                                    s.request_tasks(offset, cx);
                                }
                            }),
                        ),
                    ),
            );
        }
        let filters = div()
            .flex()
            .items_center()
            .gap_2()
            .child(input_frame(&self.search).flex_1())
            .child(
                ui_components::compact_secondary_button(
                    if self.peer_filter {
                        "当前设备"
                    } else {
                        "全部设备"
                    },
                    true,
                )
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(|s, _, _, cx| {
                        s.peer_filter = !s.peer_filter;
                        cx.notify();
                    }),
                ),
            )
            .child(
                ui_components::compact_secondary_button(
                    ["全部方向", "发送", "接收"][self.direction as usize],
                    true,
                )
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(|s, _, _, cx| {
                        s.direction = (s.direction + 1) % 3;
                        cx.notify();
                    }),
                ),
            )
            .child(
                ui_components::compact_secondary_button(
                    ["全部状态", "进行中", "暂停/中断", "已完成", "失败"]
                        [self.state_filter as usize],
                    true,
                )
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(|s, _, _, cx| {
                        s.state_filter = (s.state_filter + 1) % 5;
                        cx.notify();
                    }),
                ),
            )
            .child(
                ui_components::compact_secondary_button("重置", true).on_mouse_up(
                    MouseButton::Left,
                    cx.listener(|s, _, _, cx| {
                        s.direction = 0;
                        s.state_filter = 0;
                        s.peer_filter = false;
                        set_text(&s.search, String::new(), cx);
                        cx.notify();
                    }),
                ),
            )
            .child(
                ui_components::compact_secondary_button("清理完成历史", self.available())
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|s, _, _, cx| s.clear_history(cx)),
                    ),
            );
        let list = if filtered.is_empty() {
            div()
                .flex_1()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_2()
                .text_color(rgb(ui_theme::TEXT_SECONDARY))
                .child(
                    div()
                        .text_size(px(24.))
                        .text_color(rgb(ui_theme::TEXT_MUTED))
                        .child("▤"),
                )
                .child(div().text_sm().text_color(rgb(ui_theme::TEXT)).child(
                    if self.tasks.is_empty() {
                        "暂无传输任务"
                    } else {
                        "没有匹配的任务"
                    },
                ))
                .child(div().text_xs().child("连接设备后选择文件或文件夹开始传输"))
                .into_any_element()
        } else {
            gpui::uniform_list(
                "agent-transfer-tasks",
                filtered.len(),
                cx.processor(move |s, range: Range<usize>, _, cx| {
                    range
                        .filter_map(|index| filtered.get(index))
                        .map(|task| s.home_task_row(task, cx))
                        .collect::<Vec<_>>()
                }),
            )
            .flex_1()
            .min_h_0()
            .into_any_element()
        };
        let queue = &self.task_page["queue"];
        ui_components::card()
            .p(px(12.))
            .gap_2()
            .flex_1()
            .min_h(px(260.))
            .flex_shrink()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(ui_components::section_header(
                        "▤",
                        "文件传输",
                        "选择文件或目录，在已认证设备之间传输",
                    ))
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(ui_theme::TEXT_SECONDARY))
                            .child(format!(
                                "排队 {} · 活动文件 {}/{}",
                                queue["pending"].as_u64().unwrap_or(0),
                                queue["active_files"].as_u64().unwrap_or(0),
                                queue["limit"].as_u64().unwrap_or(1)
                            )),
                    ),
            )
            .child(toolbar)
            .child(filters)
            .child(
                div()
                    .w_full()
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .overflow_hidden()
                    .rounded_md()
                    .border_1()
                    .border_color(rgb(ui_theme::BORDER))
                    .child(list),
            )
    }

    fn home_task_row(&self, task: &Value, cx: &Context<Self>) -> gpui::Div {
        let id = task["id"].as_str().unwrap_or("").to_owned();
        let total = task["total"].as_u64().unwrap_or(0);
        let confirmed = task["confirmed"].as_u64().unwrap_or(0).min(total);
        let percent = if total == 0 {
            if task["state"] == "completed" {
                100.
            } else {
                0.
            }
        } else {
            confirmed as f32 / total as f32 * 100.
        };
        let select = id.clone();
        let mut row = div()
            .h(px(76.))
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(rgb(ui_theme::BORDER))
            .flex()
            .items_center()
            .gap_3()
            .cursor(CursorStyle::PointingHand)
            .when(self.selected.as_ref() == Some(&id), |d| {
                d.bg(rgb(ui_theme::PRIMARY_SOFT))
            })
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(move |s, _, _, cx| {
                    s.selected = Some(select.clone());
                    s.open_panel(FilePage::TaskDetails, cx);
                }),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_sm()
                            .truncate()
                            .child(task["name"].as_str().unwrap_or("").to_owned()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(rgb(ui_theme::TEXT_SECONDARY))
                            .child(format!(
                                "{} · {} · {} / {} · {}/s",
                                if task["direction"] == "send" {
                                    "发送"
                                } else {
                                    "接收"
                                },
                                task_state(task),
                                format_bytes(confirmed as f64),
                                format_bytes(total as f64),
                                format_bytes(task["rate"].as_f64().unwrap_or(0.))
                            )),
                    )
                    .child(
                        div()
                            .h(px(3.))
                            .w_full()
                            .rounded_sm()
                            .bg(rgb(ui_theme::BORDER))
                            .child(
                                div()
                                    .h_full()
                                    .w(relative(percent / 100.))
                                    .bg(rgb(ui_theme::PRIMARY)),
                            ),
                    ),
            )
            .child(div().text_xs().child(format!("{percent:.0}%")));
        for (label, operation, allowed) in [
            ("暂停", 0, task["can_pause"].as_bool() == Some(true)),
            ("继续", 1, task["can_continue"].as_bool() == Some(true)),
            (
                "取消",
                2,
                task["state"] != "completed" && task["state"] != "failed",
            ),
            ("移除记录", 3, task["state"] == "completed"),
        ] {
            if !allowed {
                continue;
            }
            let id = id.clone();
            let peer = task["peer"].as_str().unwrap_or("").to_owned();
            row = row.child(
                ui_components::compact_secondary_button(label, self.available()).on_mouse_up(
                    MouseButton::Left,
                    cx.listener(move |s, _, _, cx| {
                        cx.stop_propagation();
                        let Ok(task) = TaskId::parse(&id) else {
                            return;
                        };
                        let command = match operation {
                            0 => FileManagementCommand::Pause { task },
                            1 => FileManagementCommand::Resume {
                                peer: peer.clone(),
                                task,
                            },
                            2 => FileManagementCommand::Cancel { task },
                            _ => FileManagementCommand::RemoveHistory { tasks: vec![task] },
                        };
                        s.send_file_command(command, FileReply::Action, cx);
                    }),
                ),
            );
        }
        row
    }

    fn clear_history(&mut self, cx: &mut Context<Self>) {
        let tasks = self
            .tasks
            .iter()
            .filter(|t| t["state"] == "completed")
            .filter_map(|t| TaskId::parse(t["id"].as_str()?).ok())
            .collect::<Vec<_>>();
        if tasks.is_empty() {
            self.set_message("当前页没有已完成任务。".into(), cx);
            return;
        }
        self.send_file_command(
            FileManagementCommand::RemoveHistory { tasks },
            FileReply::Action,
            cx,
        );
    }

    pub fn render_settings(&self, cx: &Context<Self>) -> Option<gpui::Div> {
        if self.host.support == FileSupport::Unavailable {
            return None;
        }
        let settings = self.settings.as_ref().unwrap_or(&Value::Null);
        let enabled = self.host.available && self.settings.is_some();
        let configured = settings["enable_files"] == true;
        let toggle = ui_components::card()
            .child(ui_components::section_header(
                "▱",
                "文件传输",
                "更改后后台节点停止，重新启动后生效",
            ))
            .child(
                ui_components::secondary_button(
                    if configured {
                        "关闭文件传输"
                    } else {
                        "启用文件传输"
                    },
                    enabled,
                )
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(move |files, _, _, cx| {
                        if files.host.available && files.host.support != FileSupport::Unavailable {
                            cx.emit(FileEvent::Setting {
                                key: "enable_files",
                                value: json!(!configured),
                                restart: true,
                            });
                        }
                    }),
                ),
            );
        let section = div().flex().flex_col().gap_3().child(toggle);
        if !self.active() {
            return Some(section);
        }
        Some(
            section
                .child(
                    ui_components::card()
                        .child(ui_components::section_header(
                            "↻",
                            "断网自动恢复",
                            "恢复前重新验证设备身份与授权",
                        ))
                        .child(
                            ui_components::secondary_button(
                                if settings["auto_resume"] == true {
                                    "关闭自动恢复"
                                } else {
                                    "开启自动恢复"
                                },
                                enabled,
                            )
                            .on_mouse_up(
                                MouseButton::Left,
                                cx.listener(|s, _, _, cx| {
                                    let enabled = s
                                        .settings
                                        .as_ref()
                                        .is_some_and(|d| d["auto_resume"] == true);
                                    s.update_setting("auto_resume", json!(!enabled), cx);
                                }),
                            ),
                        ),
                )
                .child(
                    ui_components::card()
                        .child(ui_components::section_header(
                            "⇅",
                            "文件限速",
                            "单位 KiB/s；0 表示不限速",
                        ))
                        .child(
                            div()
                                .flex()
                                .gap_3()
                                .child(div().child("上传").child(input_frame(&self.upload)))
                                .child(div().child("下载").child(input_frame(&self.download))),
                        )
                        .child(
                            ui_components::secondary_button("保存并应用限速", enabled).on_mouse_up(
                                MouseButton::Left,
                                cx.listener(
                                    |s, _, _, cx| match crate::settings::FileLimits::parse(
                                        &Self::text(&s.upload, cx),
                                        &Self::text(&s.download, cx),
                                    ) {
                                        Ok(limits) => {
                                            s.update_setting("file_limits", json!(limits), cx)
                                        }
                                        Err(error) => {
                                            s.set_message(error.into(), cx);
                                        }
                                    },
                                ),
                            ),
                        ),
                )
                .child(
                    ui_components::card()
                        .child(ui_components::section_header(
                            "▱",
                            "接收空间",
                            "为接收文件保留可用空间（MiB）",
                        ))
                        .child(input_frame(&self.margin))
                        .child(
                            ui_components::secondary_button("保存安全余量", enabled).on_mouse_up(
                                MouseButton::Left,
                                cx.listener(|s, _, _, cx| {
                                    match Self::text(&s.margin, cx)
                                        .parse::<u16>()
                                        .ok()
                                        .filter(|n| *n <= 4096)
                                    {
                                        Some(value) => {
                                            s.update_setting("receive_safety_mib", json!(value), cx)
                                        }
                                        None => {
                                            s.set_message(
                                                "接收安全余量必须为 0..4096 MiB".into(),
                                                cx,
                                            );
                                        }
                                    }
                                }),
                            ),
                        ),
                ),
        )
    }
}
