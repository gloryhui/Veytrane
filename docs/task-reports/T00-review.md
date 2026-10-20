# T00 独立审核：建立当前基线清单

- 审核日期：2026-10-20，Asia/Shanghai。
- 审核者：Codex。
- 结论：**需返修**；仅需修正报告，不要求修改生产代码或测试。
- 基线：`7fbd50a332474d9269f1210518c16721a500389c`。
- 候选：`f840dfb701f06b9405c6c8e926ea326c20c8f3f8`。
- 候选远端分支：`origin/work/node-t00-baseline`。
- 审核记录分支：`codex/review-node-t00`；本记录不改变候选提交。
- 环境：Windows 10 专业版 10.0.19045，x86_64-pc-windows-msvc；rustc/cargo 1.98.1。

## 仓库与差异核对

实际工作区为 `F:\git\p2p_file`，远端为 `https://github.com/gloryhui/p2p_file.git`。审核开始时工作区干净；执行 `git fetch origin --prune` 并用 `git ls-remote --heads origin` 核对后，远端 main 仍为上述基线，唯一位于 main 之外的新增任务候选为上述 T00 提交。两个旧 Issue #85 分支均已包含于 main。

候选相对基线仅新增 `docs/task-reports/T00.md`，127 行。设计基线 `eb4b9d9a0b8eb6994faa2c645cb90e841b711b30` 到本候选也只有文档差异，`src/`、`tests/`、Cargo 文件、CI 和脚本均无改动。已阅读三份节点设计/计划/任务文档、报告模板、Cargo 配置及现有 CI，并核对报告列出的入口、授权、协议、持久化与测试入口。

## 返修项

### R01：修正 T03 可复用测试的位置

位置：[T00.md 第 79 行](T00.md)。报告将 `shared_session_serves_multiple_rules_and_applies_live_changes` 放在 `src/tunnel/supervisor.rs`，实际定义位于 `src/tunnel/mod.rs:2364`，完整测试名为 `tunnel::tests::shared_session_serves_multiple_rules_and_applies_live_changes`。该测试使用 supervisor 类型，但并不属于 supervisor 测试模块。错误路径会让后续任务选错测试过滤器。

修正目标：改为实际文件及完整测试名；可重跑命令为 `cargo test --locked --lib tunnel::tests::shared_session_serves_multiple_rules_and_applies_live_changes -- --exact --nocapture`。默认全目标测试已实际执行该测试，不能把筛选到 0 项当作通过。

### R02：去除历史计数差异的无证据归因

位置：[T00.md 第 115 行](T00.md)。报告写“差异来自当前工作区代码/测试数量和环境”，但上述 SHA 间没有代码或测试变更，不能把数量差异解释为源码变化。源码确实存在平台条件编译，例如 `src/storage/mod.rs:1384` 的 Unix 专属测试，以及桌面 config、task_store 等模块的 Unix 专属测试；本轮没有历史运行的完整测试清单，不能精确还原全部数量差异。

修正目标：保留本次实测计数，明确源码未变；说明 Windows 与历史平台的编译测试集合可能不同，未逐项比对历史清单的差额保持未核实。将“测试是否编译/执行”与“执行后因网络环境通过/失败”分开。补列下节四个历史失败的完整测试名及本次结果，避免仅按总数量比较。

## 独立执行的证据

复跑日志保存在被 Git 忽略的 `target/t00-review-1.log` 至 `target/t00-review-7.log`，未覆盖工作 Agent 的 `target/t00-*.log`。原始日志可在本机查看；本记录中的命令和结果摘要是持久证据。

| 完整命令 | 退出码 | 独立结果 | 日志 |
| --- | --- | --- | --- |
| `cargo fmt --all -- --check` | 0 | 通过 | `target/t00-review-1.log` |
| `cargo check --locked --all-targets` | 0 | 通过 | `target/t00-review-2.log` |
| `cargo clippy --locked --all-targets -- -D warnings` | 0 | 通过 | `target/t00-review-3.log` |
| `cargo test --locked --all-targets --no-fail-fast` | 0 | 核心 363 通过、0 失败；集成 2 + 1 + 1 通过、0 失败 | `target/t00-review-4.log` |
| `cargo test --locked --features gui --lib --no-fail-fast` | 0 | 660 通过、0 失败、0 忽略、0 筛除 | `target/t00-review-5.log` |
| `cargo check --locked --features gui --bin p2p-desktop` | 0 | 通过 | `target/t00-review-6.log` |
| `cargo clippy --locked --features gui --all-targets -- -D warnings` | 0 | 通过 | `target/t00-review-7.log` |

GUI 三条命令均有依赖 `proc-macro-error2 v2.0.1` 的 Rust 未来兼容性提示，退出码为 0；与原报告提示一致。没有新增失败。

默认二进制目标中的 0 测试结果未作为覆盖依据。默认全目标测试与 GUI 库测试中，以下历史失败均实际执行并通过：

| 历史失败完整测试名 | 本次 Windows 结果 |
| --- | --- |
| `nat::classify::tests::rfc5780_test_ii与_test_i相同则两步判定_eim` | 默认通过；GUI 通过 |
| `nat::classify::tests::rfc5780_test_iii与_test_ii相同则判定_adm` | 默认通过；GUI 通过 |
| `nat::classify::tests::rfc5780_test_iii与_test_ii不同则判定_apdm` | 默认通过；GUI 通过 |
| `nat::stun::tests::解析服务器地址` | 默认通过；GUI 通过 |

已核对三项 RFC5780 测试确实走 `127.0.0.2` 绑定，以及 DNS 测试确实断言 `256.256.256.256:1` 解析失败。本轮没有改变代理、DNS、回环地址或全局网络设置。这些 Windows 结果不证明历史环境已恢复，也不证明产品缺陷已修复。

D 检查：`git diff --check origin/main...f840dfb701f06b9405c6c8e926ea326c20c8f3f8` 通过；独立扫描已跟踪 Markdown 的文件相对链接共 95 条、缺失 0（不验证页内锚点）；T00–T04 依赖与任务边界核对通过。报告所述常量与 schema 版本均与源码一致。

## 审核边界与后续

本候选未修复 BUG-01/02/03，本轮只审核 T00 基线交付。源码仍包含信令读取错误后退出 serve_session、规则 apply 先删除旧监听再尝试新增、Web 增删行前未收集草稿的旧路径。三项缺陷的独立进程/浏览器复现仍由 T01–T03 分别交付并审核，本轮未以静态阅读代替复现或关闭缺陷。桌面 background 中名称含 `active_business_survives_signal_loss` 的测试仅验证状态投影，不构成真实业务不断流证据。

真实浏览器、Unix SIGHUP、Linux/macOS 原生交互、跨平台 CI 及真实双 NAT/四设备仍未验；不属于 T00 在本 Windows 环境必须完成的实测。实际模型别名 `opencode-go/deepseek-v4.1-flash` 与任务文档 `deepseek-v4.1-flash-1017` 的对应关系无法从 Git 独立核实，后续执行环境应明确记录。

工作 Agent 下一步只在原任务分支追加报告修正提交，提供新的完整 SHA 后重新审核。无需为本次文档返修重复运行已通过且源码未变化的全套检查；新增修改或新失败另行判断。T00 尚未通过且未合并，**T01 及后续任务保持未解锁**。合并、部署和 Issue 关闭另需用户明确授权。
