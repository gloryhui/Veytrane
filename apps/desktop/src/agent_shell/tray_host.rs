//! Desktop lifetime only. The Agent remains independent when the desktop quits.
use super::{workbench::Page, *};
use status::{CoreState, StatusSnapshot};
use tray::{NativeTray, TrayAction};
#[cfg(all(test, target_os = "windows", feature = "node"))]
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
    if available && native_window::hide_window(window).is_ok() {
        let mut children = Vec::new();
        for handle in cx.windows() {
            if handle.window_id() == window.window_handle().window_id() {
                continue;
            }
            let hidden = handle
                .update(cx, |_, child, _| {
                    #[cfg(any(target_os = "windows", target_os = "macos"))]
                    {
                        native_window::hide_window(child)
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
        let _ = handle.update(cx, |_, window, _| native_window::show_window(window));
    }
    let _ = main.update(cx, |_, window, cx| {
        native_window::show_window(window);
        window.activate_window();
        cx.activate(true);
    });
    cx.update_global::<TrayRuntime, _>(|runtime, _| {
        runtime.hidden = false;
        runtime.hidden_children.clear();
    });
}

fn snapshot(shell: &AgentShell, cx: &App) -> StatusSnapshot {
    let node = &shell.state["node"];
    let peers = node["peers"].as_object().map_or(0, |peers| {
        peers.values().filter(|p| p["phase"] == "connected").count()
    });
    #[cfg(feature = "desktop-files")]
    let application_activity = shell.files.read(cx).tray_activity();
    #[cfg(not(feature = "desktop-files"))]
    let application_activity = None;
    let speed = shell.speed_running(cx);
    let forwarding = shell.state["forwards"]["active_streams"]
        .as_u64()
        .unwrap_or(0)
        > 0;
    let core = if !shell.attached {
        CoreState::Offline
    } else if shell.state["core_running"] == false || node["signal"] == "failed" {
        CoreState::Error
    } else if application_activity.is_some() || forwarding || speed {
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
        application_activity,
        speed_running: speed,
        device_id: node["short_id"]
            .as_str()
            .or(node["node_id"].as_str())
            .unwrap_or("")
            .into(),
        signal: node["signal"].as_str().unwrap_or("未知").into(),
        tunnels: node["tunnels"]
            .as_object()
            .map_or(0, |t| t.values().filter(|s| **s == "running").count()),
        detail: shell.connection_note.to_string(),
        ..Default::default()
    }
}

fn poll(cx: &mut App) {
    let shell = cx.global::<TrayRuntime>().shell.clone();
    let status = snapshot(shell.read(cx), cx);
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

pub(super) fn request_quit(cx: &mut App) {
    if cx.has_global::<TrayRuntime>() {
        if cx.global::<TrayRuntime>().quitting {
            return;
        }
        cx.update_global::<TrayRuntime, _>(|runtime, _| runtime.quitting = true);
    }
    cx.quit();
}
