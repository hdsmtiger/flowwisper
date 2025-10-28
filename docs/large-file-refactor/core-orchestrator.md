# `core/src/orchestrator` 拆分设计

## 现状概述
`core/src/orchestrator/mod.rs` 聚合了实时引擎编排的所有逻辑：
- 引擎配置、`SpeechEngine`/`SentencePolisher` trait 以及默认实现；
- 会话生命周期启动、监控、队列/缓冲、句子选择状态机；
- 本地/云端转写调度、抛光任务、降级与通知下发；
- Whisper 本地引擎适配层与模型下载逻辑；
- 大量单元测试，覆盖实时流程、回退策略和 Whisper 缓存下载。

单文件 3,755 行，类型定义、运行态逻辑与平台集成高度耦合，维护和测试都很吃力。

## 依赖分析
- **上游调用方**：
  - `core/src/session/mod.rs` 通过 `EngineOrchestrator`、`RealtimeSessionHandle`、`RealtimeSessionConfig`、`TranscriptionUpdate`、`SessionNotice` 等类型驱动音频帧推送、增量文本消费和通知展示。
  - `core/src/session/mod.rs` 的测试同样依赖这些公开类型来模拟会话与降级流程。
- **下游依赖**：
  - 运行时逻辑需要 `crate::telemetry::events::{record_dual_view_latency, record_dual_view_revert}` 记录句子延迟与回退事件。
  - 默认引擎会在 `local-asr` feature 下使用 Whisper（`whisper_rs`），并依赖文件系统 / HTTP 下载模型；在 feature 关闭时回退到内部 `FallbackSpeechEngine`。
  - Tokio 异步运行时 (`tokio::spawn`, `mpsc`, `time`) 支撑实时任务，`async_trait` 为引擎/抛光器 trait 提供异步接口。

## 拆分目标
- 将纯数据/配置结构迁移到 `config`、`types` 子模块，保持对外 API 清晰并可复用。
- 将 `SpeechEngine`、`SentencePolisher` trait 及默认实现（轻量抛光器、回退引擎）集中到 `traits`/`engine`，方便替换实现。
- 把实时会话运行态（缓冲、进度、工人协程、监控器、云回退电路）整理到 `runtime/` 目录，由入口函数负责组装并返回 `RealtimeSessionHandle`。
- 将 Whisper 平台适配独立为 `engine/whisper.rs`，隔离平台下载逻辑，维持 feature gating。
- 将原有测试根据模块职责拆分到 `tests/` 目录，覆盖：运行态流程、选择同步、云回退、Whisper 缓存下载等场景。
- 新的 `mod.rs` 仅组合子模块并 `pub use` 既有公共类型，保持 `core::session` 等上游模块的导入不变。

## 重构步骤
1. 新建 `core/src/orchestrator/` 下列文件：`config.rs`、`types.rs`、`traits.rs`、`engine.rs`、`runtime/mod.rs`（再细分 `handle.rs`、`state.rs`、`worker.rs` 等）、`engine/whisper.rs`、`tests/*.rs`。
2. 按功能迁移原始代码：
   - 配置 & 类型定义进入 `config.rs`、`types.rs`；
   - trait 与默认实现放入 `traits.rs`（抛光器）与 `engine.rs`（回退引擎、编排入口）。
   - 运行态逻辑移至 `runtime` 子模块（缓冲、进度、工作协程、监控器、云回退电路、辅助函数）。
   - Whisper 相关实现和测试移动到 `engine/whisper.rs` 与 `engine/tests_whisper.rs`。
3. 在 `mod.rs` 中声明子模块并重新导出公共 API，使现有调用无需调整路径。
4. 更新/迁移单元测试，使用新的模块路径引用内部实现；确保所有测试覆盖恢复。
5. 运行 `cargo fmt --all`、`cargo test --manifest-path core/Cargo.toml` 验证编译与行为。
