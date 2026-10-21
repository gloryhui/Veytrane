//! Native secondary windows share one IPC model, never a second Agent worker.
use super::{workbench::Page, *};

struct PanelWindow {
    shell: Entity<AgentShell>,
    page: Page,
}

#[cfg(all(test, target_os = "windows"))]
mod tests {
    use super::*;

    #[test]
    #[ignore = "opens native windows; run explicitly on Windows"]
    fn native_secondary_windows_share_state_and_reopen_independently() {
        let root =
            std::env::temp_dir().join(format!("p2p-window-test-{:032x}", rand::random::<u128>()));
        let paths = AgentPaths::new(&root).unwrap();
        let result = std::sync::Arc::new(std::sync::Mutex::new(None));
        let result_after = result.clone();
        Application::new().run(move |cx| {
            let worker = ClientWorker::start(paths.clone()).unwrap();
            let shell = cx.new(|cx| AgentShell::new(paths, worker, cx));
            let main_shell = shell.clone();
            let main = cx
                .open_window(WindowOptions::default(), |_, _| main_shell)
                .unwrap();
            shell.update(cx, |s, cx| {
                s.windows.insert(Page::Home, main.into());
                for page in [
                    Page::Settings,
                    Page::Management,
                    Page::Diagnostics,
                    Page::Speed,
                    Page::TaskDetails,
                    Page::DirectoryPreview,
                ] {
                    s.open_panel(page, cx);
                }
            });
            let timer = cx.background_executor().clone();
            cx.spawn(async move |cx| {
                // Allow real window roots to render and subscribe to the model.
                timer.timer(Duration::from_millis(300)).await;
                let _ = cx.update(|cx| {
                    let check = (|| -> Result<(), String> {
                        if cx.windows().len() != 7 {
                            return Err("expected main + six independent windows".into());
                        }
                        let handles = shell.read(cx).windows.clone();
                        for (page, handle) in &handles {
                            if *page == Page::Home {
                                continue;
                            }
                            let same_model = handle
                                .read::<PanelWindow, _, _>(cx, |view, cx| {
                                    view.read(cx).shell == shell
                                })
                                .map_err(|e| e.to_string())?;
                            if !same_model {
                                return Err("secondary window owns a different IPC model".into());
                            }
                        }
                        if shell.read(cx).peer == shell.read(cx).management_peer
                            || shell.read(cx).signal == shell.read(cx).management_signal
                        {
                            return Err("editable layout state is shared across windows".into());
                        }
                        open_panel_window(shell.clone(), Page::Settings, cx);
                        if cx.windows().len() != 7 {
                            return Err("repeated open duplicated a live window".into());
                        }
                        let old = handles[&Page::Settings];
                        old.update(cx, |_, window, _| window.remove_window())
                            .map_err(|e| e.to_string())?;
                        if cx.windows().len() != 6 || !cx.windows().contains(&main.into()) {
                            return Err("closing settings removed another window".into());
                        }
                        shell.update(cx, |s, _| {
                            s.message = "shared state survives closing settings".into();
                        });
                        open_panel_window(shell.clone(), Page::Settings, cx);
                        let replacement = shell.read(cx).windows[&Page::Settings];
                        if replacement == old || cx.windows().len() != 7 {
                            return Err("closed settings window was not recreated".into());
                        }
                        let message = replacement
                            .read::<PanelWindow, _, _>(cx, |view, cx| {
                                view.read(cx).shell.read(cx).message.clone()
                            })
                            .map_err(|e| e.to_string())?;
                        if message != "shared state survives closing settings" {
                            return Err("shared model state was reset".into());
                        }
                        for handle in cx.windows() {
                            if handle != main.into() {
                                handle
                                    .update(cx, |_, window, _| window.remove_window())
                                    .map_err(|e| e.to_string())?;
                            }
                        }
                        if cx.windows() != vec![main.into()] {
                            return Err("main did not survive closing all secondary windows".into());
                        }
                        Ok(())
                    })();
                    *result.lock().unwrap() = Some(check);
                    cx.quit();
                });
            })
            .detach();
        });
        let outcome = result_after
            .lock()
            .unwrap()
            .take()
            .expect("native window test did not finish");
        let _ = std::fs::remove_dir_all(root);
        outcome.unwrap();
    }
}

impl Render for PanelWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.shell
            .update(cx, |shell, cx| shell.render_panel(self.page, window, cx))
    }
}

impl Page {
    fn title(self) -> &'static str {
        match self {
            Self::Home => "Veytrane",
            Self::Settings => "Veytrane · 设置",
            Self::Management => "Veytrane · 节点管理",
            Self::Diagnostics => "Veytrane · 连接诊断",
            Self::Speed => "Veytrane · 速度测试",
            Self::TaskDetails => "Veytrane · 任务详情",
            Self::DirectoryPreview => "Veytrane · 目录预览",
        }
    }

    fn dimensions(self) -> (f32, f32) {
        match self {
            Self::Home => (1180., 780.),
            Self::Settings => (900., 780.),
            Self::Management => (1180., 900.),
            Self::Diagnostics => (900., 520.),
            Self::Speed => (800., 360.),
            Self::TaskDetails => (900., 480.),
            Self::DirectoryPreview => (1000., 480.),
        }
    }
}

fn open_panel_window(shell: Entity<AgentShell>, page: Page, cx: &mut App) {
    let existing = shell.read(cx).windows.get(&page).copied();
    if let Some(handle) = existing
        && cx.windows().contains(&handle)
    {
        if page == Page::Home && cx.has_global::<tray_host::TrayRuntime>() {
            tray_host::show_main(cx);
            return;
        }
        let _ = handle.update(cx, |_, window, cx| {
            native_window::show_window(window);
            window.activate_window();
            cx.activate(true);
        });
        return;
    }
    let (width, height) = page.dimensions();
    let bounds = Bounds::centered(None, size(px(width), px(height)), cx);
    let focus = shell.read(cx).focus.clone();
    let panel_shell = shell.clone();
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
            cx.new(|cx| {
                cx.observe(&panel_shell, |_, _, cx| cx.notify()).detach();
                PanelWindow {
                    shell: panel_shell,
                    page,
                }
            })
        },
    );
    shell.update(cx, |s, cx| {
        match opened {
            Ok(handle) => {
                s.windows.insert(page, handle.into());
            }
            Err(error) => {
                s.message = format!("打开{}失败：{error}", page.title()).into();
            }
        }
        cx.notify();
    });
}

impl AgentShell {
    pub(super) fn open_panel(&mut self, page: Page, cx: &mut Context<Self>) {
        let shell = cx.entity();
        // open_window immediately renders its root. Defer the entire operation
        // until the current model and source window borrows have ended.
        cx.defer(move |cx| open_panel_window(shell, page, cx));
    }

    pub(super) fn return_to_main(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let shell = cx.entity();
        let closing = window.window_handle();
        cx.defer(move |cx| {
            // Restore main before closing the last secondary window, so the
            // application's last-window handler cannot quit between them.
            open_panel_window(shell, Page::Home, cx);
            let _ = closing.update(cx, |_, window, _| window.remove_window());
        });
    }

    fn render_panel(
        &mut self,
        page: Page,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        match page {
            Page::Home => return self.render_home(window, cx).into_any_element(),
            Page::Settings => return self.render_settings(cx).into_any_element(),
            Page::Management => return self.render_management(cx).into_any_element(),
            _ => (),
        }
        let content = match page {
            Page::Diagnostics => self.home_diagnostics(cx),
            Page::Speed => self.home_speed(cx),
            Page::TaskDetails => self.home_detail(cx),
            Page::DirectoryPreview => self.home_preview(cx),
            _ => unreachable!(),
        };
        div()
            .id("agent-secondary-window")
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
            .into_any_element()
    }
}
