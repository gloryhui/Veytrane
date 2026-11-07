//! Explicit native lifetime regression; never runs as part of headless tests.
use super::*;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use windows_sys::Win32::UI::WindowsAndMessaging::{IsWindowVisible, PostMessageW, WM_CLOSE};

fn hwnd(handle: gpui::AnyWindowHandle, cx: &mut App) -> Result<isize, String> {
    handle
        .update(cx, |_, window, _| {
            match HasWindowHandle::window_handle(window)
                .map_err(|e| e.to_string())?
                .as_raw()
            {
                RawWindowHandle::Win32(handle) => Ok(handle.hwnd.get()),
                _ => Err("expected a Win32 window".into()),
            }
        })
        .map_err(|e| e.to_string())?
}

#[test]
#[ignore = "opens native windows and a tray icon; requires P2P_AGENT_TEST_BINARY"]
fn native_close_hides_to_tray_restore_and_explicit_exit() {
    let root = std::env::temp_dir().join(format!("p2p-tray-test-{:032x}", rand::random::<u128>()));
    let paths = AgentPaths::new(&root).unwrap();
    crate::node::adapters::cli::initialize(&paths).unwrap();
    let identity = std::fs::read(paths.identity()).unwrap();
    let binary = std::env::var_os("P2P_AGENT_TEST_BINARY").expect("set P2P_AGENT_TEST_BINARY");
    struct Process(std::process::Child);
    impl Drop for Process {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    use std::os::windows::process::CommandExt;
    let mut process = Process(
        std::process::Command::new(binary)
            .arg("--data-dir")
            .arg(&root)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .creation_flags(0x08000000)
            .spawn()
            .unwrap(),
    );
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let client = AgentClient::new(paths.clone());
    runtime.block_on(async {
        let until = tokio::time::Instant::now() + Duration::from_secs(10);
        while client.execute(Command::Status).await.is_err() {
            assert!(
                tokio::time::Instant::now() < until,
                "test Agent did not start"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    });
    let result = Arc::new(Mutex::new(None));
    let result_after = result.clone();
    let explicit_exit = Arc::new(AtomicBool::new(false));
    let exit_after = explicit_exit.clone();
    let gui_paths = paths.clone();
    Application::new().run(move |cx| {
        let worker = ClientWorker::start(gui_paths.clone()).unwrap();
        let shell = cx.new(|cx| AgentShell::new(gui_paths, worker, cx));
        let main_shell = shell.clone();
        let main = cx
            .open_window(WindowOptions::default(), |_, _| main_shell)
            .unwrap();
        shell.update(cx, |s, _| {
            s.windows.insert(workbench::Page::Home, main.into());
        });
        install(shell.clone(), main.into(), cx);
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
        cx.on_app_quit(move |cx| {
            explicit_exit.store(cx.global::<TrayRuntime>().quitting, Ordering::SeqCst);
            async {}
        })
        .detach();
        shell.update(cx, |s, cx| s.open_panel(workbench::Page::Settings, cx));
        let timer = cx.background_executor().clone();
        cx.spawn(async move |cx| {
            let check: Result<(), String> = async {
                timer.timer(Duration::from_millis(250)).await;
                let (main_hwnd, child_hwnd) = cx
                    .update(|cx| -> Result<_, String> {
                        if !cx
                            .global::<TrayRuntime>()
                            .tray
                            .as_ref()
                            .is_some_and(NativeTray::available)
                        {
                            return Err("native tray is unavailable".into());
                        }
                        let child = shell.read(cx).windows[&workbench::Page::Settings];
                        let main_hwnd = hwnd(main.into(), cx)?;
                        let child_hwnd = hwnd(child, cx)?;
                        // Exercise the actual title-bar close path, not remove_window.
                        if unsafe { PostMessageW(main_hwnd as _, WM_CLOSE, 0, 0) } == 0 {
                            return Err("could not request native close".into());
                        }
                        Ok((main_hwnd, child_hwnd))
                    })
                    .map_err(|e| e.to_string())??;
                timer.timer(Duration::from_millis(250)).await;
                cx.update(|cx| -> Result<(), String> {
                    if cx.windows().len() != 2 || !cx.global::<TrayRuntime>().hidden {
                        return Err(
                            "close removed the main window instead of retaining it in the tray"
                                .into(),
                        );
                    }
                    if unsafe { IsWindowVisible(main_hwnd as _) } != 0
                        || unsafe { IsWindowVisible(child_hwnd as _) } != 0
                    {
                        return Err("close did not hide all desktop windows".into());
                    }
                    handle_action(TrayAction::Open, cx);
                    if unsafe { IsWindowVisible(main_hwnd as _) } == 0
                        || unsafe { IsWindowVisible(child_hwnd as _) } == 0
                    {
                        return Err("tray restore did not show the existing windows".into());
                    }
                    if cx.global::<TrayRuntime>().hidden {
                        return Err("tray hidden state was not cleared".into());
                    }
                    if unsafe { PostMessageW(child_hwnd as _, WM_CLOSE, 0, 0) } == 0 {
                        return Err("could not close the secondary window".into());
                    }
                    Ok(())
                })
                .map_err(|e| e.to_string())??;
                timer.timer(Duration::from_millis(250)).await;
                cx.update(|cx| -> Result<(), String> {
                    if cx.windows() != vec![main.into()] {
                        return Err("closing a secondary window affected the main window".into());
                    }
                    *result.lock().unwrap() = Some(Ok(()));
                    handle_action(TrayAction::Quit, cx);
                    Ok(())
                })
                .map_err(|e| e.to_string())??;
                // Bound the test if the explicit shutdown cannot complete.
                timer.timer(Duration::from_secs(15)).await;
                Err("explicit tray Quit did not terminate the application".into())
            }
            .await;
            if let Err(error) = check {
                let _ = cx.update(|cx| {
                    *result.lock().unwrap() = Some(Err(error));
                    cx.quit();
                });
            }
        })
        .detach();
    });
    let outcome = result_after
        .lock()
        .unwrap()
        .take()
        .expect("application quit before the close/restore checks completed");
    outcome.unwrap();
    assert!(
        process.0.try_wait().unwrap().is_none(),
        "desktop Quit stopped the Agent"
    );
    let response = runtime
        .block_on(client.execute(Command::Status))
        .expect("Agent IPC unavailable after desktop Quit");
    assert!(
        response.error.is_none(),
        "Agent status failed after desktop Quit"
    );
    assert_eq!(std::fs::read(paths.identity()).unwrap(), identity);
    drop(process);
    let _ = std::fs::remove_dir_all(root);
    assert!(
        exit_after.load(Ordering::SeqCst),
        "frontend exited without explicit tray Quit"
    );
}
