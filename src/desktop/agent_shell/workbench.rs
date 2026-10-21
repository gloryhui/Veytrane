//! The original desktop workbench, projected exclusively from Agent IPC DTOs.
//! This view never opens an identity, task store, network session or transfer service.
use super::*;
use crate::node::core::config::SpeedtestDirection as AgentSpeedDirection;

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq, Hash)]
pub(super) enum Page {
    #[default]
    Home,
    Settings,
    Management,
    Diagnostics,
    Speed,
    TaskDetails,
    DirectoryPreview,
}

pub(super) struct Workbench {
    pub task_page: Value,
    search: Entity<TextField>,
    relay: Entity<TextField>,
    upload: Entity<TextField>,
    download: Entity<TextField>,
    margin: Entity<TextField>,
    speed_direction: AgentSpeedDirection,
    speed_seconds: u16,
    direction: u8,
    state_filter: u8,
    peer_filter: bool,
    selected: Option<String>,
}

fn set_text(input: &Entity<TextField>, text: String, cx: &mut Context<AgentShell>) {
    input.update(cx, |field, cx| {
        field.content = text.into();
        field.selected_range = field.content.len()..field.content.len();
        cx.notify();
    });
}

impl Workbench {
    pub fn new(cx: &mut Context<AgentShell>) -> Self {
        Self {
            task_page: Value::Null,
            search: cx.new(|cx| TextField::new(cx, "搜索文件名或设备备注 / ID")),
            relay: cx.new(|cx| TextField::new(cx, "Relay 地址（可选）")),
            upload: cx.new(|cx| TextField::new(cx, "0")),
            download: cx.new(|cx| TextField::new(cx, "0")),
            margin: cx.new(|cx| TextField::new(cx, "64")),
            speed_direction: AgentSpeedDirection::Both,
            speed_seconds: 30,
            direction: 0,
            state_filter: 0,
            peer_filter: false,
            selected: None,
        }
    }

    pub fn load_settings(&self, settings: &Value, cx: &mut Context<AgentShell>) {
        for (input, value) in [
            (
                &self.relay,
                settings["relay_server"].as_str().unwrap_or("").to_owned(),
            ),
            (
                &self.upload,
                settings["file_limits"]["upload_kib"]
                    .as_u64()
                    .unwrap_or(0)
                    .to_string(),
            ),
            (
                &self.download,
                settings["file_limits"]["download_kib"]
                    .as_u64()
                    .unwrap_or(0)
                    .to_string(),
            ),
            (
                &self.margin,
                settings["receive_safety_mib"]
                    .as_u64()
                    .unwrap_or(64)
                    .to_string(),
            ),
        ] {
            set_text(input, value, cx);
        }
    }
}

fn input_frame(input: &Entity<TextField>) -> gpui::Div {
    div()
        .min_w_0()
        .border_1()
        .border_color(rgb(ui_theme::BORDER))
        .rounded_md()
        .bg(white())
        .child(input.clone())
}

fn task_state(task: &Value) -> &'static str {
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

fn bytes(value: f64) -> String {
    let value = if value.is_finite() { value.max(0.) } else { 0. };
    if value >= 1024. * 1024. * 1024. {
        format!("{:.1} GiB", value / (1024. * 1024. * 1024.))
    } else if value >= 1024. * 1024. {
        format!("{:.1} MiB", value / (1024. * 1024.))
    } else if value >= 1024. {
        format!("{:.1} KiB", value / 1024.)
    } else {
        format!("{value:.0} B")
    }
}

fn matches_task(
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

impl AgentShell {
    fn focus_workbench_field(&self, backwards: bool, window: &mut Window, cx: &Context<Self>) {
        let page = self
            .windows
            .iter()
            .find_map(|(page, handle)| {
                (handle.window_id() == window.window_handle().window_id()).then_some(*page)
            })
            .unwrap_or(Page::Home);
        let fields = match page {
            Page::Home => vec![&self.peer, &self.password, &self.ui.search],
            Page::Settings => vec![
                &self.local_password,
                &self.ui.upload,
                &self.ui.download,
                &self.ui.margin,
                &self.signal,
                &self.ui.relay,
            ],
            Page::Management => vec![
                &self.management_signal,
                &self.management_peer,
                &self.management_password,
                &self.management_address,
                &self.management_local_password,
                &self.name,
                &self.service,
                &self.port,
                &self.target,
            ],
            _ => return,
        };
        let current = fields
            .iter()
            .position(|f| f.focus_handle(cx).is_focused(window));
        let next = match current {
            Some(i) if backwards => (i + fields.len() - 1) % fields.len(),
            Some(i) => (i + 1) % fields.len(),
            None => 0,
        };
        window.focus(&fields[next].focus_handle(cx));
    }
    fn first_run_card(&self, cx: &Context<Self>) -> gpui::Div {
        ui_components::card()
            .child(self.connection_note.clone())
            .child(self.lifecycle_button(cx))
    }
    fn available(&self) -> bool {
        !self.exiting
            && self.attached
            && self.state["core_running"].as_bool() == Some(true)
            && !self.busy
            && self.pending.is_none()
    }

    fn selected_agent_peer(&self, cx: &Context<Self>) -> Option<String> {
        let input = Self::text(&self.peer, cx);
        if let Ok(peer) = NodeId::from_hex(&input) {
            return Some(peer.to_hex());
        }
        let short = ShortId::normalize(&input).ok()?.to_string();
        self.state["node"]["short_peers"][&short]
            .as_str()
            .map(str::to_owned)
    }

    fn can_send_files(&self, cx: &Context<Self>) -> bool {
        self.available()
            && self.selected_agent_peer(cx).is_some_and(|peer| {
                let state = &self.state["node"]["peers"][&peer];
                state["phase"].as_str() == Some("connected")
                    && (state["outbound_password"].as_bool() == Some(true)
                        || state["outbound_trusted"].as_bool() == Some(true))
            })
            && !self.ui.task_page["speeds"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|speed| speed["state"].as_str() == Some("Running"))
    }

    fn connect_from_home(&mut self, cx: &mut Context<Self>) {
        if !self.available() {
            return;
        }
        let peer = Self::text(&self.peer, cx);
        if NodeId::from_hex(&peer).is_err() && ShortId::normalize(&peer).is_err() {
            self.message = "请输入9位设备ID或完整设备ID。".into();
            cx.notify();
            return;
        }
        if self.state["node"]["node_id"].as_str() == Some(&peer)
            || ShortId::normalize(&peer).ok().is_some_and(|short| {
                self.state["node"]["short_id"].as_str() == Some(&short.to_string())
            })
        {
            self.message = "不能连接本机。".into();
            cx.notify();
            return;
        }
        let password = Self::text(&self.password, cx);
        set_text(&self.password, String::new(), cx);
        self.send(
            Command::Connect {
                peer,
                password: (!password.is_empty()).then_some(password),
            },
            Kind::Action,
            cx,
        );
    }

    fn home_pick(&mut self, directory: bool, incremental: bool, cx: &mut Context<Self>) {
        if !self.can_send_files(cx) {
            return;
        }
        let Some(peer) = self.selected_agent_peer(cx) else {
            return;
        };
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
                    if !s.can_send_files(cx) || s.selected_agent_peer(cx).as_ref() != Some(&peer) {
                        s.message = "连接或接收设备已变化，请重新选择。".into();
                        cx.notify();
                        return;
                    }
                    let command = if incremental {
                        let Some(path) = paths.into_iter().next() else {
                            return;
                        };
                        Command::PreviewDirectory { peer, path }
                    } else {
                        Command::Send { peer, paths }
                    };
                    s.send(command, Kind::Action, cx);
                });
            }
            Ok(Ok(None)) => (),
            _ => {
                let _ = shell.update(cx, |s, cx| {
                    s.message = "无法打开文件选择器。".into();
                    cx.notify();
                });
            }
        })
        .detach();
    }

    fn update_setting(&mut self, key: &str, value: Value, cx: &mut Context<Self>) {
        if !self.available() || self.dirty {
            self.message = "请先在节点管理中保存或放弃配置草稿。".into();
            cx.notify();
            return;
        }
        let Some(mut settings) = self.draft.clone() else {
            return;
        };
        settings[key] = value;
        self.send(
            Command::ApplyConfig {
                expected_revision: self.revision,
                settings,
            },
            Kind::Apply,
            cx,
        );
    }

    fn pick_agent_receive_root(&mut self, cx: &mut Context<Self>) {
        if !self.available() {
            return;
        }
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
                    s.update_setting("receive_root", json!(path), cx)
                });
            }
        })
        .detach();
    }

    fn lifecycle_button(&self, cx: &Context<Self>) -> gpui::Div {
        ui_components::secondary_button(
            if self.paths.config().is_file() {
                "启动后台节点"
            } else {
                "初始化并启动节点"
            },
            !self.busy,
        )
        .on_mouse_up(
            MouseButton::Left,
            cx.listener(|s, _, _, cx| {
                if s.busy || s.exiting || s.child_exit.is_some() {
                    return;
                }
                match start_agent(&s.paths, !s.paths.config().is_file()) {
                    Ok(receiver) => {
                        s.child_exit = Some(receiver);
                        s.message = "正在启动后台节点；身份和任务由 Agent 持有。".into();
                    }
                    Err(error) => s.message = format!("启动失败：{error}").into(),
                }
                cx.notify();
            }),
        )
    }

    fn home_header(&self, cx: &Context<Self>) -> gpui::Div {
        let (label, tone) = if !self.attached {
            ("后台未连接", ui_components::StatusTone::Warning)
        } else {
            match self.state["node"]["signal"].as_str().unwrap_or("") {
                "online" => ("信令在线", ui_components::StatusTone::Success),
                "disabled" => ("局域网 / IP直连", ui_components::StatusTone::Info),
                "connecting" | "reconnecting" => ("连接中", ui_components::StatusTone::Info),
                "failed" => ("网络异常", ui_components::StatusTone::Danger),
                _ => ("未配置", ui_components::StatusTone::Neutral),
            }
        };
        div()
            .h(px(56.))
            .flex_shrink_0()
            .flex()
            .items_center()
            .justify_between()
            .gap_3()
            .child(crate::desktop::ui::branding::brand_identity(
                "Veytrane",
                "设备之间，直接传文件",
            ))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(ui_components::status_badge(label, tone))
                    .child(
                        ui_components::secondary_button("连接诊断", true).on_mouse_up(
                            MouseButton::Left,
                            cx.listener(|s, _, _, cx| {
                                s.open_panel(Page::Diagnostics, cx);
                            }),
                        ),
                    )
                    .child(ui_components::secondary_button("设置", true).on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|s, _, _, cx| {
                            s.open_panel(Page::Settings, cx);
                        }),
                    )),
            )
    }

    fn home_connection(&self, cx: &Context<Self>) -> gpui::Div {
        let local = self.state["node"]["short_id"]
            .as_str()
            .and_then(|id| ShortId::normalize(id).ok())
            .map(|id| id.display())
            .unwrap_or_else(|| "等待信令分配设备 ID".into());
        let enabled = self.available();
        let connected = self.can_send_files(cx);
        let mut card = ui_components::card()
            .h(px(224.))
            .p(px(12.))
            .gap_2()
            .child(ui_components::section_header(
                "↔",
                "连接设备",
                "输入设备 ID；对端已信任本机时可免密码连接",
            ))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .text_xs()
                    .text_color(rgb(ui_theme::TEXT_SECONDARY))
                    .child("本机 ID")
                    .child(local)
                    .child(
                        div()
                            .text_color(rgb(ui_theme::PRIMARY))
                            .cursor(CursorStyle::PointingHand)
                            .child("复制")
                            .on_mouse_up(
                                MouseButton::Left,
                                cx.listener(|s, _, _, cx| {
                                    if let Some(id) = s.state["node"]["short_id"]
                                        .as_str()
                                        .or(s.state["node"]["node_id"].as_str())
                                    {
                                        cx.write_to_clipboard(ClipboardItem::new_string(
                                            id.to_owned(),
                                        ));
                                        s.message = "已复制本机设备 ID。".into();
                                        cx.notify();
                                    }
                                }),
                            ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(input_frame(&self.peer).flex_1())
                    .child(
                        ui_components::primary_button(
                            if connected { "断开" } else { "连接" },
                            enabled,
                        )
                        .on_click(cx.listener(move |s, _, _, cx| {
                            if connected {
                                if let Some(peer) = s.selected_agent_peer(cx) {
                                    s.send(Command::Disconnect { peer }, Kind::Action, cx);
                                }
                            } else {
                                s.connect_from_home(cx);
                            }
                        })),
                    ),
            )
            .child(input_frame(&self.password))
            .child(
                ui_components::compact_secondary_button("速度测试", connected).on_mouse_up(
                    MouseButton::Left,
                    cx.listener(move |s, _, _, cx| {
                        if connected {
                            s.open_panel(Page::Speed, cx);
                        }
                    }),
                ),
            );
        if let Some(peer) = self.selected_agent_peer(cx) {
            let state = &self.state["node"]["peers"][&peer];
            let trusted = self.draft.as_ref().is_some_and(|d| {
                d["trusted_devices"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .any(|d| d["node_id"].as_str() == Some(&peer))
            });
            if state["inbound_password"].as_bool() == Some(true) && !trusted {
                let generation = state["generation"].as_u64().unwrap_or(0);
                card = card.child(
                    ui_components::compact_secondary_button("信任此设备", enabled).on_mouse_up(
                        MouseButton::Left,
                        cx.listener(move |s, _, _, cx| {
                            s.send(
                                Command::TrustVerified {
                                    expected_revision: s.revision,
                                    peer: peer.clone(),
                                    generation,
                                    name: "可信设备".into(),
                                },
                                Kind::Action,
                                cx,
                            );
                        }),
                    ),
                );
            }
        }
        card
    }

    fn home_receive(&self, cx: &Context<Self>) -> gpui::Div {
        let directory = self
            .draft
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
                            self.available() && self.draft.is_some(),
                        )
                        .on_mouse_up(
                            MouseButton::Left,
                            cx.listener(|s, _, _, cx| s.pick_agent_receive_root(cx)),
                        ),
                    ),
            )
    }

    fn home_tasks(&mut self, cx: &mut Context<Self>) -> gpui::Div {
        let enabled = self.can_send_files(cx);
        let query = Self::text(&self.ui.search, cx);
        let selected_peer = self.selected_agent_peer(cx);
        let devices = self
            .draft
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
                    self.ui.direction,
                    self.ui.state_filter,
                    if self.ui.peer_filter {
                        Some(selected_peer.as_deref().unwrap_or(""))
                    } else {
                        None
                    },
                    devices,
                )
            })
            .cloned()
            .collect();
        let total = self.ui.task_page["total"].as_u64().unwrap_or(0);
        let offset = self.ui.task_page["offset"].as_u64().unwrap_or(0) as usize;
        let next = self.ui.task_page["next_offset"]
            .as_u64()
            .map(|n| n as usize);
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
                            self.draft
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
                                    .draft
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
                                    s.send(
                                        Command::Tasks {
                                            offset: offset.saturating_sub(128),
                                        },
                                        Kind::Tasks,
                                        cx,
                                    );
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
                                    s.send(Command::Tasks { offset }, Kind::Tasks, cx);
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
            .child(input_frame(&self.ui.search).flex_1())
            .child(
                ui_components::compact_secondary_button(
                    if self.ui.peer_filter {
                        "当前设备"
                    } else {
                        "全部设备"
                    },
                    true,
                )
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(|s, _, _, cx| {
                        s.ui.peer_filter = !s.ui.peer_filter;
                        cx.notify();
                    }),
                ),
            )
            .child(
                ui_components::compact_secondary_button(
                    ["全部方向", "发送", "接收"][self.ui.direction as usize],
                    true,
                )
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(|s, _, _, cx| {
                        s.ui.direction = (s.ui.direction + 1) % 3;
                        cx.notify();
                    }),
                ),
            )
            .child(
                ui_components::compact_secondary_button(
                    ["全部状态", "进行中", "暂停/中断", "已完成", "失败"]
                        [self.ui.state_filter as usize],
                    true,
                )
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(|s, _, _, cx| {
                        s.ui.state_filter = (s.ui.state_filter + 1) % 5;
                        cx.notify();
                    }),
                ),
            )
            .child(
                ui_components::compact_secondary_button("重置", true).on_mouse_up(
                    MouseButton::Left,
                    cx.listener(|s, _, _, cx| {
                        s.ui.direction = 0;
                        s.ui.state_filter = 0;
                        s.ui.peer_filter = false;
                        set_text(&s.ui.search, String::new(), cx);
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
        let queue = &self.ui.task_page["queue"];
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
            .when(self.ui.selected.as_ref() == Some(&id), |d| {
                d.bg(rgb(ui_theme::PRIMARY_SOFT))
            })
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(move |s, _, _, cx| {
                    s.ui.selected = Some(select.clone());
                    s.open_panel(Page::TaskDetails, cx);
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
                                bytes(confirmed as f64),
                                bytes(total as f64),
                                bytes(task["rate"].as_f64().unwrap_or(0.))
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
                            0 => Command::Pause { task },
                            1 => Command::Resume {
                                peer: peer.clone(),
                                task,
                            },
                            2 => Command::Cancel { task },
                            _ => Command::RemoveHistory { tasks: vec![task] },
                        };
                        s.send(command, Kind::Action, cx);
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
            self.message = "当前页没有已完成任务。".into();
            cx.notify();
            return;
        }
        self.send(Command::RemoveHistory { tasks }, Kind::Action, cx);
    }

    pub(super) fn home_preview(&self, cx: &Context<Self>) -> gpui::Div {
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
                    .child(div().flex_1().text_sm().child(format!(
                        "{} · 新增 {} · 变化 {} · 不变 {} · 待传 {}",
                        preview["name"].as_str().unwrap_or("目录"),
                        preview["added"],
                        preview["changed"],
                        preview["unchanged"],
                        bytes(preview["transfer_bytes"].as_u64().unwrap_or(0) as f64)
                    )))
                    .child(
                        ui_components::secondary_button("确认发送", self.available()).on_mouse_up(
                            MouseButton::Left,
                            cx.listener(move |s, _, _, cx| {
                                if let Some(request) = request.clone() {
                                    s.send(
                                        Command::ConfirmDirectory {
                                            peer: peer.clone(),
                                            request,
                                        },
                                        Kind::Action,
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
                                    s.send(Command::CancelDirectory { request }, Kind::Action, cx);
                                }
                            }),
                        ),
                    ),
            );
        }
        card
    }

    pub(super) fn home_detail(&self, cx: &Context<Self>) -> gpui::Div {
        let task = self
            .tasks
            .iter()
            .find(|t| t["id"].as_str() == self.ui.selected.as_deref());
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
            card = card.child(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        ui_components::secondary_button("定位文件", self.available()).on_mouse_up(
                            MouseButton::Left,
                            cx.listener(move |s, _, _, cx| {
                                if let Some(task) = id.clone() {
                                    s.send(Command::TaskLocation { task }, Kind::Location, cx);
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
                                        cx.spawn(async move |shell, cx| {
                                            if answer.await == Ok(0) {
                                                let _ = shell.update(cx, |s, cx| {
                                                    s.send(
                                                        Command::DeleteReceivedFile { task },
                                                        Kind::Action,
                                                        cx,
                                                    )
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

    pub(super) fn home_speed(&self, cx: &Context<Self>) -> gpui::Div {
        let selected = self.selected_agent_peer(cx);
        let speed = self.ui.task_page["speeds"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|s| s["peer"].as_str() == selected.as_deref());
        let running = speed.is_some_and(|s| s["state"] == "Running");
        let active = speed.filter(|_| running).cloned();
        ui_components::card()
            .child(ui_components::section_header(
                "↔",
                "速度测试",
                "测速由后台节点执行，关闭窗口后仍可查看结果",
            ))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        ui_components::secondary_button(
                            match self.ui.speed_direction {
                                AgentSpeedDirection::Both => "双向",
                                AgentSpeedDirection::Upload => "仅上传",
                                AgentSpeedDirection::Download => "仅下载",
                            },
                            !running,
                        )
                        .on_mouse_up(
                            MouseButton::Left,
                            cx.listener(move |s, _, _, cx| {
                                if !running {
                                    s.ui.speed_direction = match s.ui.speed_direction {
                                        AgentSpeedDirection::Both => AgentSpeedDirection::Upload,
                                        AgentSpeedDirection::Upload => {
                                            AgentSpeedDirection::Download
                                        }
                                        AgentSpeedDirection::Download => AgentSpeedDirection::Both,
                                    };
                                    cx.notify();
                                }
                            }),
                        ),
                    )
                    .child(
                        ui_components::secondary_button(
                            format!("{} 秒", self.ui.speed_seconds),
                            !running,
                        )
                        .on_mouse_up(
                            MouseButton::Left,
                            cx.listener(move |s, _, _, cx| {
                                if !running {
                                    s.ui.speed_seconds = match s.ui.speed_seconds {
                                        30 => 60,
                                        60 => 120,
                                        120 => 300,
                                        300 => 600,
                                        _ => 30,
                                    };
                                    cx.notify();
                                }
                            }),
                        ),
                    )
                    .child(
                        ui_components::primary_button(
                            if running {
                                "停止测速"
                            } else {
                                "开始测速"
                            },
                            self.available() && (running || self.can_send_files(cx)),
                        )
                        .on_click(cx.listener(move |s, _, _, cx| {
                            if let Some(speed) = &active {
                                if let (Some(peer), Some(task)) = (
                                    speed["peer"].as_str(),
                                    speed["id"].as_str().and_then(|id| TaskId::parse(id).ok()),
                                ) {
                                    s.send(
                                        Command::StopSpeed {
                                            peer: peer.into(),
                                            task,
                                        },
                                        Kind::Action,
                                        cx,
                                    );
                                }
                            } else if let Some(peer) = s.selected_agent_peer(cx) {
                                s.send(
                                    Command::StartSpeed {
                                        peer,
                                        direction: s.ui.speed_direction,
                                        seconds: s.ui.speed_seconds,
                                    },
                                    Kind::Action,
                                    cx,
                                );
                            }
                        })),
                    ),
            )
            .child(if let Some(speed) = speed {
                format!(
                    "{} · {}/s · RTT {} ms · 已传 {}",
                    speed["state"].as_str().unwrap_or(""),
                    bytes(speed["bytes_per_second"].as_f64().unwrap_or(0.)),
                    speed["rtt_ms"],
                    bytes(speed["bytes"].as_u64().unwrap_or(0) as f64)
                )
            } else {
                "尚未测速".into()
            })
    }

    pub(super) fn render_home(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let drop_enabled = self.can_send_files(cx);
        let tasks = self.home_tasks(cx);
        div()
            .size_full()
            .id("agent-workbench")
            .overflow_y_scroll()
            .overflow_x_hidden()
            .key_context("AgentWorkbench")
            .track_focus(&self.focus)
            .bg(rgb(ui_theme::PAGE_BG))
            .flex()
            .flex_col()
            .gap_3()
            .p(px(20.))
            .text_color(rgb(ui_theme::TEXT))
            .can_drop(move |value, _, _| {
                drop_enabled
                    && value
                        .downcast_ref::<gpui::ExternalPaths>()
                        .is_some_and(|paths| {
                            crate::node::core::drop_send::validate_paths(paths.paths()).is_ok()
                        })
            })
            .on_drop(cx.listener(|s, paths: &gpui::ExternalPaths, _, cx| {
                if s.can_send_files(cx)
                    && let Some(peer) = s.selected_agent_peer(cx)
                {
                    s.send(
                        Command::Send {
                            peer,
                            paths: paths.paths().to_vec(),
                        },
                        Kind::Action,
                        cx,
                    );
                }
            }))
            .on_action(cx.listener(|s, _: &ConnectPeer, _, cx| s.connect_from_home(cx)))
            .on_action(cx.listener(|s, _: &ChooseFiles, _, cx| s.home_pick(false, false, cx)))
            .on_action(cx.listener(|s, _: &ChooseFolder, _, cx| s.home_pick(true, false, cx)))
            .on_action(cx.listener(|s, _: &ToggleSettings, _, cx| {
                s.open_panel(Page::Settings, cx);
            }))
            .on_action(cx.listener(|s, _: &NextField, window, cx| {
                s.focus_workbench_field(false, window, cx)
            }))
            .on_action(cx.listener(|s, _: &PreviousField, window, cx| {
                s.focus_workbench_field(true, window, cx)
            }))
            .child(self.home_header(cx))
            .when(!self.attached, |page| page.child(self.first_run_card(cx)))
            .child(
                div()
                    .flex()
                    .gap_3()
                    .flex_shrink_0()
                    .child(div().flex_1().min_w_0().child(self.home_connection(cx)))
                    .child(div().flex_1().min_w_0().child(self.home_receive(cx))),
            )
            .child(tasks)
            .when(!self.previews.is_empty(), |page| {
                page.child(
                    ui_components::secondary_button("查看待确认的目录预览", true).on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|s, _, _, cx| s.open_panel(Page::DirectoryPreview, cx)),
                    ),
                )
            })
            .child(
                div()
                    .w_full()
                    .p(px(10.))
                    .rounded_md()
                    .bg(rgb(ui_theme::PRIMARY_SOFT))
                    .text_xs()
                    .text_color(rgb(ui_theme::TEXT_SECONDARY))
                    .child(self.message.clone()),
            )
    }

    pub(super) fn home_diagnostics(&self, cx: &Context<Self>) -> gpui::Div {
        let mut card = ui_components::card()
            .child(ui_components::section_header(
                "⌕",
                "连接诊断",
                "只显示后台节点已观察到的状态",
            ))
            .child(self.connection_note.clone())
            .child(format!(
                "本机完整 ID：{}",
                self.state["node"]["node_id"].as_str().unwrap_or("未知")
            ));
        if let Some(peers) = self.state["node"]["peers"].as_object() {
            for (id, peer) in peers {
                let id = id.clone();
                let select = id.clone();
                card = card.child(
                    div()
                        .flex()
                        .items_center()
                        .gap_2()
                        .text_xs()
                        .child(div().flex_1().truncate().child(format!(
                            "{} · {} · 接收授权 {} · 发送授权 {}",
                            id,
                            peer["phase"].as_str().unwrap_or("未知"),
                            peer["inbound_password"] == true || peer["inbound_trusted"] == true,
                            peer["outbound_password"] == true || peer["outbound_trusted"] == true
                        )))
                        .child(
                            ui_components::compact_secondary_button("选择设备", true).on_mouse_up(
                                MouseButton::Left,
                                cx.listener(move |s, _, _, cx| {
                                    set_text(&s.peer, select.clone(), cx);
                                    cx.notify();
                                }),
                            ),
                        ),
                );
            }
        }
        for line in self.state["diagnostics"]
            .as_array()
            .into_iter()
            .flatten()
            .rev()
            .take(8)
        {
            if let Some(line) = line.as_str() {
                card = card.child(div().text_xs().child(line.to_owned()));
            }
        }
        card
    }

    pub(super) fn render_settings(&self, cx: &Context<Self>) -> gpui::Stateful<gpui::Div> {
        let enabled = self.available() && self.draft.is_some();
        let settings = self.draft.as_ref().unwrap_or(&Value::Null);
        let mut root = div()
            .size_full()
            .id("agent-settings")
            .key_context("AgentWorkbench")
            .track_focus(&self.focus)
            .on_action(cx.listener(|s, _: &ToggleSettings, window, cx| {
                s.return_to_main(window, cx);
            }))
            .on_action(cx.listener(|s, _: &NextField, window, cx| {
                s.focus_workbench_field(false, window, cx)
            }))
            .on_action(cx.listener(|s, _: &PreviousField, window, cx| {
                s.focus_workbench_field(true, window, cx)
            }))
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_3()
            .p(px(20.))
            .bg(rgb(ui_theme::PAGE_BG))
            .text_color(rgb(ui_theme::TEXT))
            .child(
                div()
                    .h(px(56.))
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(crate::desktop::ui::branding::brand_identity(
                        "Veytrane 设置",
                        "个性化设置你的使用体验",
                    ))
                    .child(
                        ui_components::secondary_button("返回主界面", true).on_mouse_up(
                            MouseButton::Left,
                            cx.listener(|s, _, window, cx| {
                                s.return_to_main(window, cx);
                            }),
                        ),
                    ),
            )
            .child(
                ui_components::card()
                    .child(ui_components::section_header(
                        "⚙",
                        "节点管理",
                        "管理后台节点、服务授权与端口转发",
                    ))
                    .child(
                        ui_components::secondary_button("打开节点管理", true).on_mouse_up(
                            MouseButton::Left,
                            cx.listener(|s, _, _, cx| {
                                s.open_panel(Page::Management, cx);
                            }),
                        ),
                    )
                    .child("关闭窗口后后台节点继续运行。")
                    .when(!self.attached, |card| card.child(self.lifecycle_button(cx))),
            )
            .child(
                ui_components::card()
                    .child(ui_components::section_header(
                        "⌑",
                        "远程访问密码",
                        "密码由后台节点保存为派生密钥；更改后撤销现有授权",
                    ))
                    .child(input_frame(&self.local_password))
                    .child(
                        ui_components::secondary_button("保存访问密码", enabled).on_mouse_up(
                            MouseButton::Left,
                            cx.listener(|s, _, _, cx| {
                                let password = Self::text(&s.local_password, cx);
                                set_text(&s.local_password, String::new(), cx);
                                s.send(
                                    Command::SetPassword {
                                        expected_revision: s.revision,
                                        password,
                                    },
                                    Kind::Action,
                                    cx,
                                );
                            }),
                        ),
                    ),
            )
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
                                let enabled =
                                    s.draft.as_ref().is_some_and(|d| d["auto_resume"] == true);
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
                            .child(field("上传", &self.ui.upload))
                            .child(field("下载", &self.ui.download)),
                    )
                    .child(
                        ui_components::secondary_button("保存并应用限速", enabled).on_mouse_up(
                            MouseButton::Left,
                            cx.listener(|s, _, _, cx| {
                                match crate::node::core::bandwidth::FileLimits::parse(
                                    &Self::text(&s.ui.upload, cx),
                                    &Self::text(&s.ui.download, cx),
                                ) {
                                    Ok(limits) => {
                                        s.update_setting("file_limits", json!(limits), cx)
                                    }
                                    Err(error) => {
                                        s.message = error.into();
                                        cx.notify();
                                    }
                                }
                            }),
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
                    .child(input_frame(&self.ui.margin))
                    .child(
                        ui_components::secondary_button("保存安全余量", enabled).on_mouse_up(
                            MouseButton::Left,
                            cx.listener(|s, _, _, cx| {
                                match Self::text(&s.ui.margin, cx)
                                    .parse::<u16>()
                                    .ok()
                                    .filter(|n| *n <= 4096)
                                {
                                    Some(value) => {
                                        s.update_setting("receive_safety_mib", json!(value), cx)
                                    }
                                    None => {
                                        s.message = "接收安全余量必须为 0..4096 MiB".into();
                                        cx.notify();
                                    }
                                }
                            }),
                        ),
                    ),
            );
        let mut trusted = ui_components::card().child(ui_components::section_header(
            "✓",
            "可信设备",
            "撤销信任后重新验证访问权限",
        ));
        for device in settings["trusted_devices"].as_array().into_iter().flatten() {
            let peer = device["node_id"].as_str().unwrap_or("").to_owned();
            let select = peer.clone();
            trusted = trusted.child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().flex_1().truncate().child(format!(
                        "{} · {}",
                        device["display_name"].as_str().unwrap_or(""),
                        peer
                    )))
                    .child(
                        ui_components::secondary_button("连接", enabled).on_mouse_up(
                            MouseButton::Left,
                            cx.listener(move |s, _, _, cx| {
                                set_text(&s.peer, select.clone(), cx);
                                s.open_panel(Page::Home, cx);
                                s.connect_from_home(cx);
                            }),
                        ),
                    )
                    .child(
                        ui_components::secondary_button("撤销信任", enabled).on_mouse_up(
                            MouseButton::Left,
                            cx.listener(move |s, _, _, cx| {
                                if !s.available() || s.dirty {
                                    return;
                                }
                                if let Some(mut settings) = s.draft.clone() {
                                    if let Some(devices) =
                                        settings["trusted_devices"].as_array_mut()
                                    {
                                        devices.retain(|d| d["node_id"].as_str() != Some(&peer));
                                    }
                                    if let Some(sources) = settings["trust_sources"].as_object_mut()
                                    {
                                        sources.remove(&peer);
                                    }
                                    s.send(
                                        Command::ApplyConfig {
                                            expected_revision: s.revision,
                                            settings,
                                        },
                                        Kind::Apply,
                                        cx,
                                    );
                                }
                            }),
                        ),
                    ),
            );
        }
        if settings["trusted_devices"]
            .as_array()
            .is_none_or(Vec::is_empty)
        {
            trusted = trusted.child("暂无可信设备");
        }
        root = root
            .child(trusted)
            .child(
                ui_components::card()
                    .child(ui_components::section_header(
                        "⌕",
                        "高级网络设置",
                        "更改信令或 Relay 后需要停止并重新启动后台节点",
                    ))
                    .child(field("信令地址 HOST:PORT", &self.signal))
                    .child(field("Relay 地址（可选）", &self.ui.relay))
                    .child(
                        ui_components::secondary_button("保存网络并停止节点", enabled).on_mouse_up(
                            MouseButton::Left,
                            cx.listener(|s, _, _, cx| {
                                if s.dirty {
                                    s.message = "请先保存或放弃节点管理草稿。".into();
                                    cx.notify();
                                    return;
                                }
                                if let Some(mut settings) = s.draft.clone() {
                                    settings["signal_server"] = json!(Self::text(&s.signal, cx));
                                    let relay = Self::text(&s.ui.relay, cx);
                                    settings["relay_server"] = if relay.is_empty() {
                                        Value::Null
                                    } else {
                                        json!(relay)
                                    };
                                    s.send(
                                        Command::ConfigureRestart {
                                            expected_revision: s.revision,
                                            settings,
                                        },
                                        Kind::Restart,
                                        cx,
                                    );
                                }
                            }),
                        ),
                    )
                    .child(self.lifecycle_button(cx)),
            )
            .child(div().text_sm().child(self.message.clone()));
        root
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
}
