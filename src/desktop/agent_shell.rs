//! Native management window. This module holds no identity or business runtime.
use super::*;
use crate::node::{
    agent_client::AgentClient,
    agent_paths::AgentPaths,
    ipc::{Command, Request, Response},
    task_model::TaskId,
};
use serde_json::{Value, json};
use std::sync::mpsc::{self, Receiver, SyncSender};

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Status,
    Config,
    Tasks,
    Services,
    Action,
    Apply,
    Restart,
}
struct Job {
    request: Request,
    kind: Kind,
}
struct Update {
    job: Job,
    result: Result<Response, String>,
}
struct ClientWorker {
    send: SyncSender<Job>,
    receive: Receiver<Update>,
}
impl ClientWorker {
    fn start(paths: AgentPaths) -> std::io::Result<Self> {
        let (send, jobs) = mpsc::sync_channel::<Job>(32);
        let (updates, receive) = mpsc::sync_channel(64);
        std::thread::Builder::new()
            .name("p2p-management-client".into())
            .spawn(move || {
                let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                else {
                    return;
                };
                let client = AgentClient::new(paths);
                let perform = |job: Job| {
                    let result = runtime
                        .block_on(client.request(job.request.clone()))
                        .map_err(|e| e.to_string());
                    updates.send(Update { job, result }).is_ok()
                };
                for (command, kind) in [
                    (Command::Status, Kind::Status),
                    (Command::Config, Kind::Config),
                    (Command::Tasks { offset: 0 }, Kind::Tasks),
                ] {
                    if !perform(Job {
                        request: Request::new(command),
                        kind,
                    }) {
                        return;
                    }
                }
                loop {
                    match jobs.recv_timeout(Duration::from_secs(2)) {
                        Ok(job) => {
                            if !perform(job) {
                                break;
                            }
                        }
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            for (command, kind) in [
                                (Command::Status, Kind::Status),
                                (Command::Tasks { offset: 0 }, Kind::Tasks),
                            ] {
                                if !perform(Job {
                                    request: Request::new(command),
                                    kind,
                                }) {
                                    return;
                                }
                            }
                        }
                    }
                }
            })?;
        Ok(Self { send, receive })
    }
}
struct AgentShell {
    paths: AgentPaths,
    worker: ClientWorker,
    peer: Entity<TextField>,
    password: Entity<TextField>,
    local_password: Entity<TextField>,
    service: Entity<TextField>,
    port: Entity<TextField>,
    name: Entity<TextField>,
    target: Entity<TextField>,
    signal: Entity<TextField>,
    state: Value,
    tasks: Vec<Value>,
    previews: Vec<Value>,
    services: Vec<Value>,
    draft: Option<Value>,
    revision: u64,
    message: SharedString,
    connection_note: SharedString,
    busy: bool,
    active_request: Option<String>,
    dirty: bool,
    pending: Option<(Request, Kind)>,
}
impl AgentShell {
    fn new(paths: AgentPaths, worker: ClientWorker, cx: &mut Context<Self>) -> Self {
        let view = Self {
            paths,
            worker,
            peer: cx.new(|cx| TextField::new(cx, "远端设备完整ID")),
            password: cx.new(|cx| TextField::new_password(cx, "远端访问密码")),
            local_password: cx.new(|cx| TextField::new_password(cx, "本机访问密码")),
            service: cx.new(|cx| TextField::new(cx, "远端服务ID")),
            port: cx.new(|cx| TextField::new(cx, "本机端口，如18080")),
            name: cx.new(|cx| TextField::new(cx, "服务或转发名称")),
            target: cx.new(|cx| TextField::new(cx, "本机服务目标，如127.0.0.1:8080")),
            signal: cx.new(|cx| TextField::new(cx, "信令地址，如signal.example:9000")),
            state: Value::Null,
            tasks: vec![],
            previews: vec![],
            services: vec![],
            draft: None,
            revision: 0,
            message: "关闭窗口后后台节点继续运行。".into(),
            connection_note: "正在附着后台节点…".into(),
            busy: false,
            active_request: None,
            dirty: false,
            pending: None,
        };
        let background = cx.background_executor().clone();
        cx.spawn(async move |shell, cx| {
            loop {
                if shell
                    .update(cx, |shell, cx| {
                        while let Ok(update) = shell.worker.receive.try_recv() {
                            shell.receive(update, cx);
                        }
                        cx.notify();
                    })
                    .is_err()
                {
                    break;
                }
                background.timer(Duration::from_millis(200)).await;
            }
        })
        .detach();
        view
    }
    fn text(field: &Entity<TextField>, cx: &Context<Self>) -> String {
        field.read(cx).content.trim().to_owned()
    }
    fn send(&mut self, command: Command, kind: Kind, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        if self.pending.is_some() && !command.is_query() {
            self.message = "请先重试上次结果未知的请求，或刷新确认状态。".into();
            cx.notify();
            return;
        }
        self.dispatch(Request::new(command), kind, cx);
    }
    fn dispatch(&mut self, request: Request, kind: Kind, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let request_id = request.request_id.clone();
        if self.worker.send.try_send(Job { request, kind }).is_err() {
            self.message = "管理队列已满或已关闭".into();
        } else {
            self.busy = true;
            self.active_request = Some(request_id);
            self.message = "请求已提交，等待节点确认…".into();
        }
        cx.notify();
    }
    fn receive(&mut self, update: Update, cx: &mut Context<Self>) {
        let kind = update.job.kind;
        let response_id = update.job.request.request_id.clone();
        if self.active_request.as_ref() == Some(&update.job.request.request_id) {
            self.busy = false;
            self.active_request = None;
        }
        let response = match update.result {
            Ok(response) => response,
            Err(error) => {
                self.connection_note = format!("后台节点不可用：{error}").into();
                if !update.job.request.command.is_query() {
                    self.pending = Some((update.job.request, kind));
                    self.message = "请求结果未知；请重试同一请求或查询状态。".into();
                }
                return;
            }
        };
        if let Some(error) = response.error {
            if error.code == "outcome_unknown" {
                self.pending = Some((update.job.request, kind));
            }
            if error.code != "outcome_unknown"
                && self
                    .pending
                    .as_ref()
                    .is_some_and(|(r, _)| r.request_id == response_id)
            {
                self.pending = None;
            }
            self.message = format!("{}：{}", error.code, error.message).into();
            return;
        }
        let value = response.result.unwrap_or(Value::Null);
        match kind {
            Kind::Status => {
                self.state = value;
                self.connection_note = "已附着同一后台节点".into();
            }
            Kind::Tasks => {
                self.tasks = value["tasks"].as_array().cloned().unwrap_or_default();
                self.previews = value["directory_previews"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
            }
            Kind::Config => {
                self.revision = value["revision"].as_u64().unwrap_or(0);
                self.draft = Some(value["settings"].clone());
                self.dirty = false;
                if let Some(signal) = value["settings"]["signal_server"].as_str() {
                    let text = signal.to_owned();
                    self.signal.update(cx, |field, cx| {
                        field.content = text.into();
                        field.selected_range = field.content.len()..field.content.len();
                        cx.notify();
                    });
                }
            }
            Kind::Services => {
                self.services = value.as_array().cloned().unwrap_or_default();
            }
            Kind::Apply | Kind::Restart => {
                self.revision = value["revision"].as_u64().unwrap_or(self.revision);
                self.dirty = false;
                self.pending = None;
                self.message = if kind == Kind::Restart {
                    "配置已提交，节点正在停止；请点击启动节点重新上线。"
                } else {
                    "配置已提交。"
                }
                .into();
            }
            Kind::Action => {
                if let Some(revision) = value["revision"].as_u64() {
                    self.revision = revision;
                }
                self.pending = None;
                self.message = "节点已确认命令；任务结果请查看列表。".into();
            }
        }
    }
    fn add_forward(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let peer = Self::text(&self.peer, cx);
        let service = Self::text(&self.service, cx);
        let port = Self::text(&self.port, cx).parse::<u16>();
        if NodeId::from_hex(&peer).is_err()
            || crate::node::wire::validate_service_id(&service).is_err()
            || port.as_ref().is_err()
            || port.as_ref().is_ok_and(|p| *p == 0)
        {
            self.message = "请填写完整设备ID、服务ID及1..65535本机端口。".into();
            cx.notify();
            return;
        }
        if let Some(draft) = &mut self.draft {
            let id = format!("{:032x}", rand::random::<u128>());
            draft["service_ids"][&id] = json!(service);
            draft["forwards"].as_array_mut().unwrap().push(json!({"id":id,"name":Self::text(&self.name,cx).replace('\n'," "),"peer_node_id":peer,"listen":format!("127.0.0.1:{}",port.unwrap()),"target":"127.0.0.1:1","enabled":true,"auto_start":true}));
            self.dirty = true;
            self.message = "已加入草稿，保存后开始监听。".into();
            cx.notify();
        }
    }
    fn resource(&mut self, kind: &str, value: bool, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let peer = Self::text(&self.peer, cx);
        if NodeId::from_hex(&peer).is_err() {
            self.message = "请填写完整设备ID".into();
            cx.notify();
            return;
        }
        if let Some(draft) = &mut self.draft {
            let peers = draft["resources"]["peers"].as_array_mut().unwrap();
            if let Some(row) = peers
                .iter_mut()
                .find(|r| r["node_id"].as_str() == Some(&peer))
            {
                row[kind] = json!(value);
            } else {
                let mut row = json!({"node_id":peer,"files":false,"speed":false,"disabled":false});
                row[kind] = json!(value);
                peers.push(row);
            }
            self.dirty = true;
            self.message = "权限已加入草稿，保存后生效。".into();
            cx.notify();
        }
    }
    fn trust(&mut self, grant: bool, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let peer = Self::text(&self.peer, cx);
        let Ok(node) = NodeId::from_hex(&peer) else {
            self.message = "请填写完整设备ID".into();
            cx.notify();
            return;
        };
        if let Some(draft) = &mut self.draft {
            let devices = draft["trusted_devices"].as_array_mut().unwrap();
            devices.retain(|d| d["node_id"].as_str() != Some(&peer));
            if grant {
                devices.push(
                    serde_json::to_value(crate::node::trusted_devices::TrustedDevice::new(
                        node,
                        "管理员授权".into(),
                        None,
                    ))
                    .unwrap(),
                );
                draft["trust_sources"][&peer] = json!("administrator");
            } else {
                draft["trust_sources"]
                    .as_object_mut()
                    .unwrap()
                    .remove(&peer);
            }
            self.dirty = true;
            cx.notify();
        }
    }
    fn add_service(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let peer = Self::text(&self.peer, cx);
        let service = Self::text(&self.service, cx);
        let target = Self::text(&self.target, cx);
        if NodeId::from_hex(&peer).is_err()
            || crate::node::wire::validate_service_id(&service).is_err()
            || target.parse::<SocketAddr>().is_err()
        {
            self.message = "请填写完整被授权设备ID、服务ID和本机服务地址。".into();
            cx.notify();
            return;
        }
        if let Some(draft) = &mut self.draft {
            draft["services"].as_array_mut().unwrap().push(json!({"id":service,"name":Self::text(&self.name,cx),"target":target,"allowed_peers":[peer],"enabled":true}));
            self.dirty = true;
            cx.notify();
        }
    }
    fn save(&mut self, restart: bool, cx: &mut Context<Self>) {
        let Some(mut settings) = self.draft.clone() else {
            return;
        };
        let command = if restart {
            settings["signal_server"] = json!(Self::text(&self.signal, cx));
            Command::ConfigureRestart {
                expected_revision: self.revision,
                settings,
            }
        } else {
            Command::ApplyConfig {
                expected_revision: self.revision,
                settings,
            }
        };
        self.send(
            command,
            if restart { Kind::Restart } else { Kind::Apply },
            cx,
        );
    }
    fn choose_files(&mut self, directory: bool, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let peer = Self::text(&self.peer, cx);
        if NodeId::from_hex(&peer).is_err() {
            self.message = "请先填写接收设备完整ID。".into();
            cx.notify();
            return;
        }
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: !directory,
            directories: directory,
            multiple: true,
            prompt: Some("选择发送文件".into()),
        });
        cx.spawn(async move |shell, cx| {
            if let Ok(Ok(Some(paths))) = picked.await {
                let _ = shell.update(cx, |shell, cx| {
                    shell.send(Command::Send { peer, paths }, Kind::Action, cx)
                });
            }
        })
        .detach();
    }
}
fn field(label: &'static str, input: &Entity<TextField>) -> gpui::Div {
    div().flex().flex_col().gap_1().flex_1().child(label).child(
        div()
            .border_1()
            .border_color(rgb(ui_theme::BORDER))
            .rounded_md()
            .child(input.clone()),
    )
}
impl Render for AgentShell {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let enabled = !self.busy;
        let mut root = div()
            .id("agent-page")
            .size_full()
            .overflow_y_scroll()
            .flex()
            .flex_col()
            .gap_4()
            .p_6()
            .bg(rgb(ui_theme::PAGE_BG))
            .text_color(rgb(ui_theme::TEXT))
            .child(div().text_2xl().child("P2P File · 节点管理"))
            .child(self.connection_note.clone())
            .child(div().text_sm().child(format!(
                "本机：{} · 信令：{} · 草稿 revision {}{}",
                self.state["node"]["node_id"].as_str().unwrap_or("未连接"),
                self.state["node"]["signal"].as_str().unwrap_or("未知"),
                self.revision,
                if self.dirty {
                    " · 有未保存草稿"
                } else {
                    ""
                }
            )))
            .child(
                div()
                    .flex()
                    .gap_2()
                    .flex_wrap()
                    .child(
                        ui_components::primary_button("初始化并启动节点", enabled).on_click(
                            cx.listener(|shell, _, _, cx| {
                                if shell.busy {
                                    return;
                                }
                                match start_agent(&shell.paths, true) {
                                    Ok(()) => {
                                        shell.message = "已请求启动；等待身份锁与IPC就绪。".into()
                                    }
                                    Err(e) => shell.message = e.to_string().into(),
                                }
                                cx.notify();
                            }),
                        ),
                    )
                    .child(
                        ui_components::primary_button("读取配置（放弃草稿）", enabled).on_click(
                            cx.listener(|s, _, _, cx| s.send(Command::Config, Kind::Config, cx)),
                        ),
                    )
                    .child(
                        ui_components::primary_button("停止后台节点", enabled).on_click(
                            cx.listener(|s, _, _, cx| s.send(Command::Stop, Kind::Action, cx)),
                        ),
                    )
                    .child(
                        ui_components::primary_button(
                            "重试未确认请求",
                            enabled && self.pending.is_some(),
                        )
                        .on_click(cx.listener(|s, _, _, cx| {
                            if let Some((request, kind)) = s.pending.clone() {
                                s.dispatch(request, kind, cx);
                            }
                        })),
                    ),
            )
            .child(
                ui_components::card()
                    .child("信令设置（明确停止并重新启动后生效）")
                    .child(field("信令地址", &self.signal))
                    .child(
                        ui_components::primary_button(
                            "保存信令并停止节点",
                            enabled && self.draft.is_some(),
                        )
                        .on_click(cx.listener(|s, _, _, cx| s.save(true, cx))),
                    ),
            )
            .child(
                ui_components::card()
                    .child("连接与授权")
                    .child(
                        div()
                            .flex()
                            .gap_3()
                            .child(field("远端设备 ID", &self.peer))
                            .child(field("远端密码", &self.password)),
                    )
                    .child(
                        div()
                            .flex()
                            .gap_2()
                            .child(ui_components::primary_button("连接设备", enabled).on_click(
                                cx.listener(|s, _, _, cx| {
                                    let peer = Self::text(&s.peer, cx);
                                    let password = Self::text(&s.password, cx);
                                    s.password.update(cx, |field, cx| {
                                        field.content = "".into();
                                        field.selected_range = 0..0;
                                        cx.notify();
                                    });
                                    s.send(
                                        Command::Connect {
                                            peer,
                                            password: if password.is_empty() {
                                                None
                                            } else {
                                                Some(password)
                                            },
                                        },
                                        Kind::Action,
                                        cx,
                                    );
                                }),
                            ))
                            .child(ui_components::primary_button("断开设备", enabled).on_click(
                                cx.listener(|s, _, _, cx| {
                                    s.send(
                                        Command::Disconnect {
                                            peer: Self::text(&s.peer, cx),
                                        },
                                        Kind::Action,
                                        cx,
                                    )
                                }),
                            ))
                            .child(
                                ui_components::primary_button("查询授权服务", enabled).on_click(
                                    cx.listener(|s, _, _, cx| {
                                        s.send(
                                            Command::Services {
                                                peer: Self::text(&s.peer, cx),
                                            },
                                            Kind::Services,
                                            cx,
                                        )
                                    }),
                                ),
                            ),
                    )
                    .child(field("设置本机访问密码", &self.local_password))
                    .child(
                        ui_components::primary_button("保存密码并撤销现有授权", enabled).on_click(
                            cx.listener(|s, _, _, cx| {
                                let password = Self::text(&s.local_password, cx);
                                s.local_password.update(cx, |f, cx| {
                                    f.content = "".into();
                                    f.selected_range = 0..0;
                                    cx.notify();
                                });
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
            .child(div().text_sm().child(self.message.clone()));
        let mut directory = ui_components::card().child("远端授权服务（点击选择）");
        for service in &self.services {
            let id = service["id"].as_str().unwrap_or("").to_owned();
            let text = format!("{} · {}", service["name"].as_str().unwrap_or(""), id);
            directory = directory.child(ui_components::primary_button(text, enabled).on_click(
                cx.listener(move |s, _, _, cx| {
                    s.service.update(cx, |field, cx| {
                        field.content = id.clone().into();
                        field.selected_range = field.content.len()..field.content.len();
                        cx.notify();
                    });
                }),
            ));
        }
        let mut permissions =
            ui_components::card().child("上述设备的本机资源授权（加入草稿后保存）");
        for (label, kind, value) in [
            ("允许文件访问", "files", true),
            ("撤销文件访问", "files", false),
            ("允许测速", "speed", true),
            ("禁用设备", "disabled", true),
            ("重新启用设备", "disabled", false),
        ] {
            permissions = permissions.child(
                ui_components::primary_button(label, enabled)
                    .on_click(cx.listener(move |s, _, _, cx| s.resource(kind, value, cx))),
            );
        }
        permissions = permissions
            .child(
                ui_components::primary_button("管理员信任此设备", enabled)
                    .on_click(cx.listener(|s, _, _, cx| s.trust(true, cx))),
            )
            .child(
                ui_components::primary_button("撤销此设备信任", enabled)
                    .on_click(cx.listener(|s, _, _, cx| s.trust(false, cx))),
            );
        root = root.child(directory).child(permissions);
        let mut forwards = ui_components::card()
            .child("本机转发与提供的服务")
            .child(
                div()
                    .flex()
                    .gap_3()
                    .child(field("名称", &self.name))
                    .child(field("服务 ID", &self.service))
                    .child(field("本机转发端口", &self.port)),
            )
            .child(
                ui_components::primary_button("新增本机转发到草稿", enabled)
                    .on_click(cx.listener(|s, _, _, cx| s.add_forward(cx))),
            )
            .child(field("本机提供的服务地址", &self.target))
            .child(
                ui_components::primary_button("提供服务给上述设备（加入草稿）", enabled)
                    .on_click(cx.listener(|s, _, _, cx| s.add_service(cx))),
            );
        if let Some(draft) = &self.draft {
            for rule in draft["forwards"].as_array().into_iter().flatten() {
                let id = rule["id"].as_str().unwrap_or("").to_owned();
                let start = id.clone();
                let stop = id.clone();
                let delete = id.clone();
                forwards =
                    forwards.child(
                        div()
                            .flex()
                            .gap_2()
                            .items_center()
                            .child(format!(
                                "{} · {} → {}",
                                rule["name"].as_str().unwrap_or(""),
                                rule["listen"].as_str().unwrap_or(""),
                                draft["service_ids"][&id].as_str().unwrap_or("legacy")
                            ))
                            .child(
                                ui_components::primary_button(format!("启动 {id}"), enabled)
                                    .on_click(cx.listener(move |s, _, _, cx| {
                                        s.send(
                                            Command::StartForward {
                                                rule: start.clone(),
                                            },
                                            Kind::Action,
                                            cx,
                                        )
                                    })),
                            )
                            .child(
                                ui_components::primary_button(format!("停止 {id}"), enabled)
                                    .on_click(cx.listener(move |s, _, _, cx| {
                                        s.send(
                                            Command::StopForward { rule: stop.clone() },
                                            Kind::Action,
                                            cx,
                                        )
                                    })),
                            )
                            .child(
                                ui_components::primary_button(format!("删除 {id}"), enabled)
                                    .on_click(cx.listener(move |s, _, _, cx| {
                                        if s.busy {
                                            return;
                                        }
                                        if let Some(draft) = &mut s.draft {
                                            draft["forwards"]
                                                .as_array_mut()
                                                .unwrap()
                                                .retain(|r| r["id"].as_str() != Some(&delete));
                                            draft["service_ids"]
                                                .as_object_mut()
                                                .unwrap()
                                                .remove(&delete);
                                            s.dirty = true;
                                            cx.notify();
                                        }
                                    })),
                            ),
                    );
            }
        }
        if let Some(draft) = &self.draft {
            for service in draft["services"].as_array().into_iter().flatten() {
                let id = service["id"].as_str().unwrap_or("").to_owned();
                forwards = forwards.child(
                    div()
                        .flex()
                        .gap_2()
                        .items_center()
                        .child(format!(
                            "提供服务 {} → {}",
                            id,
                            service["target"].as_str().unwrap_or("")
                        ))
                        .child(
                            ui_components::primary_button(format!("删除服务 {id}"), enabled)
                                .on_click(cx.listener(move |s, _, _, cx| {
                                    if s.busy {
                                        return;
                                    }
                                    if let Some(d) = &mut s.draft {
                                        d["services"]
                                            .as_array_mut()
                                            .unwrap()
                                            .retain(|row| row["id"].as_str() != Some(&id));
                                        s.dirty = true;
                                        cx.notify();
                                    }
                                })),
                        ),
                );
            }
        }
        root = root.child(
            forwards.child(
                ui_components::primary_button("保存配置草稿", enabled && self.dirty)
                    .on_click(cx.listener(|s, _, _, cx| s.save(false, cx))),
            ),
        );
        let mut files = ui_components::card()
            .child("文件任务（来自后台持久记录）")
            .child(
                ui_components::primary_button("选择文件并发送给上述设备", enabled)
                    .on_click(cx.listener(|s, _, _, cx| s.choose_files(false, cx))),
            );
        files = files.child(
            ui_components::primary_button("选择文件夹并预览", enabled)
                .on_click(cx.listener(|s, _, _, cx| s.choose_files(true, cx))),
        );
        for preview in &self.previews {
            let request = preview["id"].as_str().unwrap_or("").to_owned();
            let cancel = request.clone();
            let peer = preview["peer"].as_str().unwrap_or("").to_owned();
            // Peer is projected as canonical text by the Agent DTO.
            files = files.child(
                div()
                    .flex()
                    .gap_2()
                    .items_center()
                    .child(format!(
                        "目录预览：新增{}，变化{}，不变{}，待传{}字节",
                        preview["added"],
                        preview["changed"],
                        preview["unchanged"],
                        preview["transfer_bytes"]
                    ))
                    .child(
                        ui_components::primary_button(format!("确认目录 {request}"), enabled)
                            .on_click(cx.listener(move |s, _, _, cx| {
                                if let Ok(request) = TaskId::parse(&request) {
                                    s.send(
                                        Command::ConfirmDirectory {
                                            peer: peer.clone(),
                                            request,
                                        },
                                        Kind::Action,
                                        cx,
                                    );
                                }
                            })),
                    )
                    .child(
                        ui_components::primary_button(format!("取消预览 {cancel}"), enabled)
                            .on_click(cx.listener(move |s, _, _, cx| {
                                if let Ok(request) = TaskId::parse(&cancel) {
                                    s.send(Command::CancelDirectory { request }, Kind::Action, cx);
                                }
                            })),
                    ),
            );
        }
        for task in &self.tasks {
            let id = task["id"].as_str().unwrap_or("").to_owned();
            let peer = task["peer"].as_str().unwrap_or("").to_owned();
            let mut row = div().flex().gap_2().items_center().child(format!(
                "{} · {} · {}/{}",
                task["name"].as_str().unwrap_or(""),
                task["state"].as_str().unwrap_or(""),
                task["confirmed"],
                task["total"]
            ));
            for (label, operation) in [("暂停", 0), ("继续", 1), ("取消", 2)] {
                let id = id.clone();
                let peer = peer.clone();
                row = row.child(
                    ui_components::primary_button(format!("{label} {id}"), enabled).on_click(
                        cx.listener(move |s, _, _, cx| {
                            if let Ok(task) = TaskId::parse(&id) {
                                let command = match operation {
                                    0 => Command::Pause { task },
                                    1 => Command::Resume {
                                        peer: peer.clone(),
                                        task,
                                    },
                                    _ => Command::Cancel { task },
                                };
                                s.send(command, Kind::Action, cx);
                            }
                        }),
                    ),
                );
            }
            files = files.child(row);
        }
        root.child(files)
    }
}
pub(crate) fn start_agent(paths: &AgentPaths, initialize: bool) -> std::io::Result<()> {
    let executable = std::env::current_exe()?.with_file_name(if cfg!(windows) {
        "p2p-agent.exe"
    } else {
        "p2p-agent"
    });
    let mut command = std::process::Command::new(executable);
    command.arg("--data-dir").arg(&paths.root);
    if initialize {
        command.arg("--initialize");
    }
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let mut child = command.spawn()?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}
pub fn run(root: PathBuf, background: bool) {
    let paths = match AgentPaths::new(&root) {
        Ok(paths) => paths,
        Err(e) => {
            eprintln!("管理目录不可用：{e}");
            return;
        }
    };
    if background {
        if let Err(e) = start_agent(&paths, false) {
            eprintln!("启动后台节点失败：{e}");
        }
        return;
    }
    let worker = match ClientWorker::start(paths.clone()) {
        Ok(worker) => worker,
        Err(e) => {
            eprintln!("IPC客户端不可用：{e}");
            return;
        }
    };
    Application::new().run(move |cx: &mut App| {
        cx.bind_keys([
            KeyBinding::new("backspace", Backspace, None),
            KeyBinding::new("delete", Delete, None),
            KeyBinding::new("left", Left, None),
            KeyBinding::new("right", Right, None),
            KeyBinding::new("shift-left", SelectLeft, None),
            KeyBinding::new("shift-right", SelectRight, None),
            KeyBinding::new("secondary-a", SelectAll, None),
            KeyBinding::new("secondary-v", Paste, None),
            KeyBinding::new("secondary-c", Copy, None),
            KeyBinding::new("secondary-x", Cut, None),
            KeyBinding::new("home", Home, None),
            KeyBinding::new("end", End, None),
        ]);
        let bounds = Bounds::centered(None, size(px(1180.), px(900.)), cx);
        if let Err(e) = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |_, cx| cx.new(|cx| AgentShell::new(paths, worker, cx)),
        ) {
            eprintln!("打开窗口失败：{e}");
            cx.quit();
        }
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        cx.activate(true);
    });
}
