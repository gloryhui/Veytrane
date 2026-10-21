# Agent 桌面与后台生命周期

当前桌面仅使用 Agent IPC。内嵌旧桌面、`--legacy` 参数及其登录启动注册已移除。

- 关闭主窗口：托盘可用时隐藏窗口；不可用时保留任务栏入口。Agent 继续运行。
- 托盘打开：恢复主窗口及此前隐藏的二级窗口。
- 托盘状态面板与设置：打开独立的诊断或设置窗口；关闭子窗口不会停止 Agent。
- 明确退出（托盘或 Ctrl/Cmd+Q）：请求停止 Agent，等待所有权释放后退出。
- `p2p-desktop --background`：尝试启动同目录 Agent 后结束桌面启动进程，不创建隐藏的旧桌面。
- 常驻与登录启动使用[节点宿主管理](../packaging/node/README.md)。

## 验证

Linux X11 原生窗口与模拟 SNI host 冒烟：

```sh
cargo build --locked --features gui --bin p2p-desktop
dbus-run-session -- /usr/bin/python3 scripts/desktop-background-smoke.py \
  --binary "$PWD/target/debug/p2p-desktop" --output /tmp/new-background-evidence
```

脚本使用隔离 D-Bus/Xvfb 和空的节点目录，不创建真实身份、连接公网或修改登录项。
覆盖关闭隐藏、原窗口恢复、独立子窗口关闭/重开、托盘 host 消失后恢复和明确退出。
它不证明实际 macOS/Windows 原生桌面、Wayland 或系统登录行为。
