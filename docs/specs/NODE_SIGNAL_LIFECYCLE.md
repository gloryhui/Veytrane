# T06：P1 信令与业务生命周期契约

- 状态：本轮实现规格已冻结，测试验收后置；不代表 P1 已验收。
- 日期：2026-10-20；范围：现有 CLI 单 peer，不引入新线上协议或多 peer 能力。
- 实施路径：`src/net/signaling.rs`、`src/net/mod.rs`、`src/tunnel/mod.rs`。

## 所有权

serve_session 持有业务 network、认证连接任务、Relay offer 与探测任务。新 ResilientSignaling 持有唯一后台信令 worker，worker 独占当前 SignalingClient 的 reader/writer；消费者只有一个有界事件队列。worker 只发布事件，不直接修改业务连接。Drop 中取消 worker，SignalingClient 随之析构取消 reader/writer。复用原身份、信令 TLS 选项和当前 network 的候选；不创建第二份 UDP/QUIC 网络、不生成身份、不降级 TLS 或业务认证。

## 状态转移

| 当前 | 事件 | 动作 | 下一状态 |
| --- | --- | --- | --- |
| 已登记 | 有效信令消息 | 带当前代际投递，队列容量 64 | 已登记 |
| 已登记 | 读取失败/EOF | 递增代际，丢弃当前客户端，保留健康 Direct/Relay 业务 | 退避 |
| 退避 | 延迟结束 | 同身份和 TLS 配置重新登记；单次连接/注册使用既有有界超时 | 重连中 |
| 重连中 | 登记失败 | 记录诊断；延迟 1/2/4/8/16/30 秒，最大 30 秒 | 退避 |
| 重连中 | 登记成功 | 查询原 peer 以取得新 pairing，成功后重置退避 | 已登记 |
| 任意 | 宿主退出/取消 | 析构 worker，释放 reader/writer/退避/连接未来 | 停止 |

消费者对照共享当前代际，丢弃旧登记积压事件。真正 QUIC/业务错误仍按现有业务逻辑处理；不得因为信令 EOF break serve 循环。业务空闲重打洞仍遵循既有 re_punch_after，健康认证连接存在时不提前关闭 network。后台 worker 异常停止时，serve 记录一次错误并停用事件分支，继续已有业务，避免封闭通道空转。

Direct/Relay 共用信令恢复；重连不删除当前有效 Relay offer 或连接。新 token 仍经原有身份、来源及并发限制路由。信令恢复不复活已撤销业务权限。

## 后续验收向量

T02 独立进程脚本：持续 TCP 字节流及周期请求中只停信令；应持续，恢复后新 peer 会话应成功。另验重连连败、TLS/主机名拒绝无降级、旧代消息丢弃、业务空闲重打洞、显式退出与 worker 清理，以及 Direct/Relay 原回归。本轮只编译/静态检查，以上均待执行。
