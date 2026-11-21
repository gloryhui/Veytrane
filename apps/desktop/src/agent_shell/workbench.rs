//! The original desktop workbench, projected exclusively from Agent IPC DTOs.
//! This view never opens an identity, task store, network session or transfer service.
use super::*;
use ui_components::input_frame;
use veytrane_desktop_ui::format_bytes;
use veytrane_management::protocol::SpeedtestDirection as AgentSpeedDirection;

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq, Hash)]
pub(super) enum Page {
    #[default]
    Home,
    Settings,
    Management,
    Diagnostics,
    Speed,
}

pub(super) struct Workbench {
    relay: Entity<TextField>,
    speed_direction: AgentSpeedDirection,
    speed_seconds: u16,
}

fn speed_supported(state: &Value) -> bool {
    state
        .get("capabilities")
        .and_then(Value::as_object)
        .is_none_or(|capabilities| capabilities.get("speed").and_then(Value::as_bool) == Some(true))
}

fn speed_start_ready(supported: bool, connected: bool, running: bool) -> bool {
    supported && connected && !running
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
            relay: cx.new(|cx| TextField::new(cx, "Relay 地址（可选）")),
            speed_direction: AgentSpeedDirection::Both,
            speed_seconds: 30,
        }
    }

    pub fn load_settings(&self, settings: &Value, cx: &mut Context<AgentShell>) {
        set_text(
            &self.relay,
            settings["relay_server"].as_str().unwrap_or("").to_owned(),
            cx,
        );
    }
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
            Page::Home => vec![&self.peer, &self.password],
            Page::Settings => vec![&self.local_password, &self.signal, &self.ui.relay],
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
        #[cfg(feature = "desktop-files")]
        let fields = {
            let mut fields = fields;
            let files = self.files.read(cx);
            match page {
                Page::Home => fields.extend(files.search_field()),
                Page::Settings => fields.splice(1..1, files.settings_fields()).for_each(drop),
                _ => (),
            }
            fields
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
    pub(super) fn available(&self) -> bool {
        !self.exiting
            && self.attached
            && self.state["core_running"].as_bool() == Some(true)
            && !self.busy
            && self.pending.is_none()
    }

    pub(super) fn selected_agent_peer(&self, cx: &Context<Self>) -> Option<String> {
        let input = Self::text(&self.peer, cx);
        if let Ok(peer) = NodeId::from_hex(&input) {
            return Some(peer.to_hex());
        }
        let short = ShortId::normalize(&input).ok()?.to_string();
        self.state["node"]["short_peers"][&short]
            .as_str()
            .map(str::to_owned)
    }

    pub(super) fn speed_supported(&self) -> bool {
        speed_supported(&self.state)
    }

    pub(super) fn peer_connected(&self, cx: &Context<Self>) -> bool {
        self.available()
            && self.selected_agent_peer(cx).is_some_and(|peer| {
                let state = &self.state["node"]["peers"][&peer];
                state["phase"].as_str() == Some("connected")
                    && (state["outbound_password"].as_bool() == Some(true)
                        || state["outbound_trusted"].as_bool() == Some(true))
            })
    }

    fn speed_rows<'a>(&'a self, _cx: &'a App) -> &'a [Value] {
        let rows = self.state["speeds"].as_array().map(Vec::as_slice);
        #[cfg(feature = "desktop-files")]
        let rows = rows.or_else(|| self.files.read(_cx).legacy_speed_rows());
        rows.unwrap_or_default()
    }

    pub(super) fn speed_running(&self, cx: &App) -> bool {
        self.speed_rows(cx)
            .iter()
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
        let connected = self.peer_connected(cx);
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
            .child(input_frame(&self.password));
        if self.speed_supported() {
            card = card.child(
                ui_components::compact_secondary_button("速度测试", connected).on_mouse_up(
                    MouseButton::Left,
                    cx.listener(move |s, _, _, cx| {
                        if s.peer_connected(cx) {
                            s.open_panel(Page::Speed, cx);
                        }
                    }),
                ),
            );
        }
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

    pub(super) fn home_speed(&self, cx: &Context<Self>) -> gpui::Div {
        let connected = self.peer_connected(cx);
        let any_speed_running = self.speed_running(cx);
        let selected = self.selected_agent_peer(cx);
        let speed = self
            .speed_rows(cx)
            .iter()
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
                            self.speed_supported() && !running,
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
                            self.speed_supported() && !running,
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
                            self.available()
                                && (running
                                    || speed_start_ready(
                                        self.speed_supported(),
                                        connected,
                                        any_speed_running,
                                    )),
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
                            } else if speed_start_ready(
                                s.speed_supported(),
                                s.peer_connected(cx),
                                s.speed_running(cx),
                            ) && let Some(peer) = s.selected_agent_peer(cx)
                            {
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
                    format_bytes(speed["bytes_per_second"].as_f64().unwrap_or(0.)),
                    speed["rtt_ms"],
                    format_bytes(speed["bytes"].as_u64().unwrap_or(0) as f64)
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
        let page = div()
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
            .on_action(cx.listener(|s, _: &ConnectPeer, _, cx| s.connect_from_home(cx)))
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
            .when(!self.attached, |page| page.child(self.first_run_card(cx)));
        let page = page
            .child(self.home_connection(cx))
            .child(self.home_forwarding(self.selected_agent_peer(cx), cx));
        #[cfg(feature = "desktop-files")]
        let page = self
            .files
            .update(cx, |files, cx| files.mount_home(page, cx));
        page.child(
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

    pub(super) fn render_settings(&self, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
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
            );
        #[cfg(feature = "desktop-files")]
        {
            root = root.children(self.files.update(cx, |files, cx| files.render_settings(cx)));
        }
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
    fn speed_capability_and_connection_are_both_required() {
        assert!(speed_supported(&json!({})));
        assert!(speed_supported(&json!({"capabilities":{"speed":true}})));
        assert!(!speed_supported(&json!({"capabilities":{"speed":false}})));
        assert!(!speed_start_ready(false, true, false));
        assert!(!speed_start_ready(true, false, false));
        assert!(speed_start_ready(true, true, false));
        assert!(!speed_start_ready(true, true, true));
    }

    #[test]
    fn bytes_keeps_precision_and_clamps_invalid_values() {
        assert_eq!(format_bytes(1023.), "1023 B");
        assert_eq!(format_bytes(1024.), "1.0 KiB");
        assert_eq!(format_bytes(1.5 * 1024. * 1024.), "1.5 MiB");
        assert_eq!(format_bytes(2.25 * 1024. * 1024. * 1024.), "2.2 GiB");
        assert_eq!(format_bytes(-1.), "0 B");
        assert_eq!(format_bytes(f64::NAN), "0 B");
        assert_eq!(format_bytes(f64::INFINITY), "0 B");
    }
}
