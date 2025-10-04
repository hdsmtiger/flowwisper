# Flowwisper Core Service

Flowwisper Core Service 是桌面端守护进程的脚手架，实现热键监听、音频采集、引擎编排、持久化与同步等核心能力。本目录提供了最小可运行的 Rust 工程骨架，后续可在各模块内补充具体实现。主要结构如下：

- `audio/`：音频采集与预处理（CoreAudio/WASAPI、VAD、降噪）。
- `session/`：会话状态机、流程编排、与桌面壳层的事件通信。
- `orchestrator/`：本地/云端识别引擎选择与调用策略。
- `persistence/`：SQLCipher + FTS5 历史记录与片段库存储。
- `telemetry/`：统一的 Tracing 日志、指标初始化。

运行命令：

```bash
cargo run
```
