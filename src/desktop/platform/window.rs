//! Native window visibility used by both desktop shells.
use gpui::Window;
#[cfg(target_os = "linux")]
use std::collections::HashSet;

#[cfg(any(target_os = "macos", target_os = "windows"))]
pub(in crate::desktop) fn hide_window(window: &mut Window) -> Result<(), String> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let handle = HasWindowHandle::window_handle(window)
        .map_err(|e| e.to_string())?
        .as_raw();
    match handle {
        #[cfg(target_os = "macos")]
        RawWindowHandle::AppKit(handle) => {
            // GPUI owns the NSView and NSWindow; orderOut changes visibility only.
            unsafe {
                use objc2::{msg_send, runtime::AnyObject};
                let native: *mut AnyObject =
                    msg_send![handle.ns_view.as_ptr() as *mut AnyObject, window];
                let _: () = msg_send![native, orderOut: std::ptr::null::<AnyObject>()];
            }
            Ok(())
        }
        #[cfg(target_os = "windows")]
        RawWindowHandle::Win32(handle) => {
            unsafe {
                windows_sys::Win32::UI::WindowsAndMessaging::ShowWindow(
                    handle.hwnd.get() as _,
                    windows_sys::Win32::UI::WindowsAndMessaging::SW_HIDE,
                );
            }
            Ok(())
        }
        _ => Err("当前窗口系统不支持后台隐藏，保持主窗口".into()),
    }
}

pub(in crate::desktop) fn show_window(window: &mut Window) {
    #[cfg(target_os = "linux")]
    if gpui::guess_compositor() == "X11" {
        let _ = map_x11_main(true);
    }
    #[cfg(target_os = "windows")]
    {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        if let Ok(handle) = HasWindowHandle::window_handle(window)
            && let RawWindowHandle::Win32(handle) = handle.as_raw()
        {
            unsafe {
                windows_sys::Win32::UI::WindowsAndMessaging::ShowWindow(
                    handle.hwnd.get() as _,
                    windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOW,
                );
            }
        }
    }
    let _ = window;
}

#[cfg(target_os = "linux")]
pub(in crate::desktop) fn hide_window(window: &mut Window) -> Result<(), String> {
    if gpui::guess_compositor() == "X11" {
        map_x11_main(false)
    } else {
        window.minimize_window();
        Ok(())
    }
}

#[cfg(target_os = "linux")]
fn map_x11_main(show: bool) -> Result<(), String> {
    use x11rb::{
        connection::Connection,
        protocol::xproto::{AtomEnum, ConnectionExt},
    };
    // GPUI 0.2.2's X11 HasWindowHandle implementation is unimplemented. Find
    // only our main client via its PID and exact title, retaining the GPUI
    // window rather than removing the last window (which stops its event loop).
    let (connection, screen) = x11rb::connect(None).map_err(|e| e.to_string())?;
    let root = connection.setup().roots[screen].root;
    let atom = |name: &[u8]| -> Result<u32, String> {
        Ok(connection
            .intern_atom(false, name)
            .map_err(|e| e.to_string())?
            .reply()
            .map_err(|e| e.to_string())?
            .atom)
    };
    let pid_atom = atom(b"_NET_WM_PID")?;
    let clients_atom = atom(b"_NET_CLIENT_LIST")?;
    let clients = connection
        .get_property(false, root, clients_atom, AtomEnum::WINDOW, 0, 4096)
        .map_err(|e| e.to_string())?
        .reply()
        .map_err(|e| e.to_string())?;
    let mut pending: std::collections::VecDeque<u32> =
        clients.value32().into_iter().flatten().collect();
    pending.extend(
        connection
            .query_tree(root)
            .map_err(|e| e.to_string())?
            .reply()
            .map_err(|e| e.to_string())?
            .children,
    );
    let mut visited = HashSet::new();
    while let Some(candidate) = pending.pop_front() {
        if visited.len() >= 4096 {
            break;
        }
        if !visited.insert(candidate) {
            continue;
        }
        let pid = connection
            .get_property(false, candidate, pid_atom, AtomEnum::CARDINAL, 0, 1)
            .map_err(|e| e.to_string())?
            .reply();
        if let Ok(pid) = pid
            && pid.value32().and_then(|mut values| values.next()) == Some(std::process::id())
        {
            let title = connection
                .get_property(false, candidate, AtomEnum::WM_NAME, AtomEnum::ANY, 0, 32)
                .map_err(|e| e.to_string())?
                .reply();
            if title.is_ok_and(|title| title.value == b"Veytrane") {
                let request = if show {
                    connection.map_window(candidate)
                } else {
                    connection.unmap_window(candidate)
                }
                .map_err(|e| e.to_string())?;
                request.check().map_err(|e| e.to_string())?;
                return connection.flush().map_err(|e| e.to_string());
            }
        }
        // Window managers can reparent a hidden client into a frame; without
        // EWMH, root descendants are the fallback. Never modify other windows.
        if let Ok(request) = connection.query_tree(candidate)
            && let Ok(tree) = request.reply()
        {
            pending.extend(
                tree.children
                    .into_iter()
                    .take(4096usize.saturating_sub(visited.len())),
            );
        }
    }
    Err("未找到本进程的主窗口，保持现有窗口".into())
}
