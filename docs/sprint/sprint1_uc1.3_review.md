# Sprint 1 UC1.3 架构复检

## 复检结论
✅ 当前实现已对齐 400 ms SLA 判定与 Karabiner 退化路径，探针日志能够准确反映慢链路并覆盖 macOS 兜底；建议继续扩展设备矩阵验证以巩固数据。

## 评审范围
- 需求依据：《Flowwisper Fn 语音输入助手 PRD》关于 400 ms 内反馈的体验要求以及 Fn 失败时的备用组合指引。【F:docs/voice_fn_transcriber_prd.md†L97-L114】
- 架构依据：《技术架构设计》中 Hotkey Compatibility Layer 必须依赖原生驱动回调并提供 Raw Input 等退化矩阵的约束。【F:docs/architecture.md†L92-L107】
- 实现依据：`native_probe.rs` 的平台探测逻辑、`main.rs` 中兼容层的探测/回退实现以及 `App.tsx` 中的前端判定流程。【F:apps/desktop/src-tauri/src/native_probe.rs†L265-L360】【F:apps/desktop/src-tauri/src/main.rs†L404-L488】【F:apps/desktop/src/App.tsx†L101-L193】

## 对齐情况概览
| 能力 | 预期 | 当前实现 | 结论 |
| --- | --- | --- | --- |
| Fn 延迟测量 | 以驱动时间戳衡量“事件→监听”耗时，确保指标仅反映系统处理链路而非用户反应时间。【F:docs/sprint/sprint1.md†L18-L21】 | 探针统一以 `reaction`（提示→驱动时间戳）对比 400 ms SLA，同时保留 `latency` 作为回调诊断指标，慢链路会准确标记为超时并写入原因字段。【F:apps/desktop/src-tauri/src/native_probe.rs†L36-L52】【F:apps/desktop/src-tauri/src/native_probe.rs†L214-L274】【F:apps/desktop/src-tauri/src/native_probe.rs†L676-L739】 | ✅ 达成 |
| 驱动退化路径 | Hotkey Compatibility Layer 需在原生失败时退化至 Raw Input（Win）或 Karabiner（macOS），并记录覆盖矩阵。【F:docs/architecture.md†L95-L106】 | macOS 侧在 IOHID 超时或未捕获时主动接入 Karabiner 虚拟设备并合并原因日志，Windows 仍保留 Raw Input 兜底，探针结果会标注 `IOHID/Karabiner` 接口及退化结论。【F:apps/desktop/src-tauri/src/native_probe.rs†L214-L330】【F:apps/desktop/src-tauri/src/native_probe.rs†L332-L416】【F:apps/desktop/src-tauri/src/native_probe.rs†L676-L739】 | ✅ 达成 |
| 400 ms 反馈体验 | 在 400 ms SLA 内给出状态反馈，超标时提供诊断但允许用户决策。【F:docs/voice_fn_transcriber_prd.md†L97-L104】 | 前端提示与风险决策改为依赖 `reaction`，慢链路会显示“驱动回调耗时”并保留 Fn 选项，同时写入回退原因，满足透明提示与自助决策要求。【F:apps/desktop/src/App.tsx†L103-L185】 | ✅ 达成 |

## 主要 Gap 详情
1. ✅ **SLA 判定指标已修正** —— `within_sla` 现以 `reaction` 对比 400 ms SLA，并在日志中保留 `latency` 供诊断，慢链路会生成“驱动回调耗时”原因字段。【F:apps/desktop/src-tauri/src/native_probe.rs†L36-L52】【F:apps/desktop/src-tauri/src/native_probe.rs†L214-L274】

2. ✅ **macOS Karabiner 退化就绪** —— IOHID 未命中时会自动注册 Karabiner 虚拟设备，成功/失败都会合并原因，双失败时返回 `IOHID/Karabiner` 接口说明兜底缺失。【F:apps/desktop/src-tauri/src/native_probe.rs†L214-L330】

3. ✅ **前端提示依赖正确指标** —— Onboarding UI 改为依据 `reaction` 与 `within_sla` 呈现 SLA 提示，慢链路将显示“驱动回调耗时”并允许用户自愿继续使用 Fn。【F:apps/desktop/src/App.tsx†L103-L185】

## 后续建议
- ✅ **修正计时模型**：已完成，需在回归测试中关注 `reaction` 与日志对齐情况。
- ✅ **补齐 macOS 退化实现**：已完成，建议在 QA 矩阵中记录 Karabiner 退化命中率。
- ✅ **更新前端判定逻辑**：已完成，建议观察慢链路下用户选择占比。
- 🔄 **扩展验证矩阵**：建议持续跟踪不同键盘/远程会话场景的探针日志，确保数据闭环。
