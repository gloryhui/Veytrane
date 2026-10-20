//! Desktop lifetime only. The Agent remains independent until explicit Quit.
use super::{workbench::Page, *};
use background::{CoreState, StatusSnapshot};
use tray::{NativeTray, TrayAction};
#[cfg(all(test, target_os = "windows"))]
#[path = "tray_host_tests.rs"]
mod tests;

pub(super) struct TrayRuntime {
    shell: Entity<AgentShell>,
    main: gpui::AnyWindowHandle,
    tray: Option<NativeTray>,
    hidden: bool,
    hidden_children: Vec<gpui::AnyWindowHandle>,
    quitting: bool,
}
impl gpui::Global for TrayRuntime {}

pub(super) fn install(shell: Entity<AgentShell>, main: gpui::AnyWindowHandle, cx: &mut App) {
    cx.set_global(TrayRuntime {
        shell,
        main,
        tray: None,
        hidden: false,
        hidden_children: vec![],
        quitting: false,
    });
    let _ = main.update(cx, |_, window, cx| {
        window.on_window_should_close(cx, close_main);
    });
    #[cfg(any(target_os = "windows", target_os = "macos"))]
    finish_install(NativeTray::new(), cx);
    #[cfg(target_os = "linux")]
    cx.spawn(async move |cx| {
        let result = cx
            .background_executor()
            .spawn(async { NativeTray::new() })
            .await;
        let _ = cx.update(|cx| finish_install(result, cx));
    })
    .detach();
    let timer = cx.background_executor().clone();
    cx.spawn(async move |cx| {
        loop {
            timer.timer(Duration::from_millis(200)).await;
            if cx.update(poll).is_err() {
                break;
            }
        }
    })
    .detach();
}

fn finish_install(result: Result<NativeTray, String>, cx: &mut App) {
    match result {
        Ok(tray) => cx.update_global::<TrayRuntime, _>(|runtime, _| runtime.tray = Some(tray)),
        Err(error) => {
            let shell = cx.global::<TrayRuntime>().shell.clone();
            shell.update(cx, |s, cx| {
                s.message = format!("托盘暂不可用；关闭主窗口时保留在任务栏：{error}").into();
                cx.notify();
            });
        }
    }
}

fn close_main(window: &mut Window, cx: &mut App) -> bool {
    if cx.global::<TrayRuntime>().quitting {
        return true;
    }
    let available = cx
        .global::<TrayRuntime>()
        .tray
        .as_ref()
        .is_some_and(NativeTray::available);
    if available && background::hide_window(window).is_ok() {
        let mut children = Vec::new();
        for handle in cx.windows() {
            if handle.window_id() == window.window_handle().window_id() {
                continue;
            }
            let hidden = handle
                .update(cx, |_, child, _| {
                    #[cfg(any(target_os = "windows", target_os = "macos"))]
                    {
                        background::hide_window(child)
                    }
                    #[cfg(target_os = "linux")]
                    {
                        child.minimize_window();
                        Ok::<_, String>(())
                    }
                })
                .is_ok_and(|result| result.is_ok());
            if hidden {
                children.push(handle);
            }
        }
        cx.update_global::<TrayRuntime, _>(|runtime, _| {
            runtime.hidden = true;
            runtime.hidden_children = children;
        });
    } else {
        // Keep a reachable taskbar entry if the OS cannot supply a tray icon.
        window.minimize_window();
    }
    false
}

pub(super) fn show_main(cx: &mut App) {
    if !cx.has_global::<TrayRuntime>() {
        return;
    }
    let runtime = cx.global::<TrayRuntime>();
    let main = runtime.main;
    let children = runtime.hidden_children.clone();
    for handle in children {
        let _ = handle.update(cx, |_, window, _| background::show_window(window));
    }
    let _ = main.update(cx, |_, window, cx| {
        background::show_window(window);
        window.activate_window();
        cx.activate(true);
    });
    cx.update_global::<TrayRuntime, _>(|runtime, _| {
        runtime.hidden = false;
        runtime.hidden_children.clear();
    });
}

fn snapshot(shell: &AgentShell) -> StatusSnapshot {
    let node = &shell.state["node"];
    let peers = node["peers"].as_object().map_or(0, |peers| {
        peers.values().filter(|p| p["phase"] == "connected").count()
    });
    let active = shell.ui.task_page["queue"]["active_files"]
        .as_u64()
        .unwrap_or(0) as usize;
    let speed = shell.ui.task_page["speeds"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|s| s["state"] == "Running");
    let core = if !shell.attached {
        CoreState::Offline
    } else if shell.state["core_running"] == false || node["signal"] == "failed" {
        CoreState::Error
    } else if active > 0 || speed {
        CoreState::Active
    } else if peers > 0 || node["signal"] == "online" {
        CoreState::Online
    } else if node["signal"] == "connecting" || node["signal"] == "reconnecting" {
        CoreState::Connecting
    } else {
        CoreState::Offline
    };
    StatusSnapshot {
        core,
        connected_peers: peers,
        active_files: active,
        speed_running: speed,
        device_id: node["short_id"]
            .as_str()
            .or(node["node_id"].as_str())
            .unwrap_or("")
            .into(),
        signal: node["signal"].as_str().unwrap_or("未知").into(),
        queued_files: shell.ui.task_page["queue"]["pending"].as_u64().unwrap_or(0) as usize,
        bytes_per_second: shell
            .tasks
            .iter()
            .map(|t| t["rate"].as_f64().unwrap_or(0.).max(0.) as u64)
            .fold(0u64, u64::saturating_add),
        tunnels: node["tunnels"]
            .as_object()
            .map_or(0, |t| t.values().filter(|s| **s == "running").count()),
        detail: shell.connection_note.to_string(),
        ..Default::default()
    }
}

fn poll(cx: &mut App) {
    let shell = cx.global::<TrayRuntime>().shell.clone();
    let status = snapshot(shell.read(cx));
    let (actions, restore) = cx.update_global::<TrayRuntime, _>(|runtime, _| {
        if let Some(tray) = &mut runtime.tray {
            let actions = tray.poll();
            let failed = tray.update(status).is_err() || !tray.available();
            (actions, failed && runtime.hidden)
        } else {
            (vec![], runtime.hidden)
        }
    });
    if restore {
        show_main(cx);
    }
    for action in actions {
        handle_action(action, cx);
    }
}

fn handle_action(action: TrayAction, cx: &mut App) {
    match action {
        TrayAction::Open => show_main(cx),
        TrayAction::Settings | TrayAction::Panel => {
            show_main(cx);
            let shell = cx.global::<TrayRuntime>().shell.clone();
            shell.update(cx, |s, cx| {
                s.open_panel(
                    if matches!(action, TrayAction::Settings) {
                        Page::Settings
                    } else {
                        Page::Diagnostics
                    },
                    cx,
                )
            });
        }
        TrayAction::Quit => request_quit(cx),
    }
}

async fn stopped(paths: &AgentPaths) -> bool {
    crate::node::management_cli::run(crate::node::management_cli::Args {
        data_dir: Some(paths.root.clone()),
        command: crate::node::management_cli::Action::OwnershipStatus,
    })
    .await
    .is_ok()
}

async fn stop_for_exit(paths: AgentPaths) -> Result<(), String> {
    if stopped(&paths).await {
        return Ok(());
    }
    let response = AgentClient::new(paths.clone()).execute(Command::Stop).await;
    match response {
        Ok(response) => {
            if let Some(error) = response.error {
                return Err(error.message);
            }
        }
        Err(error) => {
            if !stopped(&paths).await {
                return Err(error.to_string());
            }
        }
    }
    let until = tokio::time::Instant::now() + Duration::from_secs(10);
    while tokio::time::Instant::now() < until {
        if stopped(&paths).await {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    Err("后台仍在保存或退出，请稍后重试；未强制结束进程。".into())
}

fn request_quit(cx: &mut App) {
    if cx.global::<TrayRuntime>().quitting {
        return;
    }
    let shell = cx.global::<TrayRuntime>().shell.clone();
    if shell.read(cx).ui.migrating() {
        shell.update(cx, |s, cx| {
            s.message = "正在迁移身份和任务，请完成后再退出。".into();
            cx.notify();
        });
        show_main(cx);
        return;
    }
    cx.update_global::<TrayRuntime, _>(|runtime, _| runtime.quitting = true);
    let paths = shell.update(cx, |s, cx| {
        s.exiting = true;
        s.message = "正在保存后台状态并退出…".into();
        cx.notify();
        s.paths.clone()
    });
    let executor = cx.background_executor().clone();
    cx.spawn(async move |cx| {
        let result = executor
            .spawn(async move {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|e| e.to_string())?
                    .block_on(stop_for_exit(paths))
            })
            .await;
        let _ = cx.update(|cx| match result {
            Ok(()) => cx.quit(),
            Err(error) => {
                cx.update_global::<TrayRuntime, _>(|runtime, _| runtime.quitting = false);
                shell.update(cx, |s, cx| {
                    s.exiting = false;
                    s.message = format!("退出未完成：{error}").into();
                    cx.notify();
                });
                show_main(cx);
            }
        });
    })
    .detach();
}
