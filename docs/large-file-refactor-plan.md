# 大文件拆分方案

## 扫描概览
- 执行命令：`python - <<'PY' ...`（内联脚本，过滤首方代码扩展名，忽略 `.git`、`node_modules` 等目录）。
- 发现 11 个首方代码文件超过 800 行，统计如下：

| 文件 | 行数 |
| --- | ---: |
| `core/src/orchestrator/mod.rs` | 3755 |
| `apps/desktop/src/App.tsx` | 2063 |
| `core/src/session/mod.rs` | 2056 |
| `apps/desktop/src-tauri/src/audio.rs` | 1861 |
| `apps/desktop/src-tauri/src/hotkey.rs` | 1836 |
| `apps/desktop/src/features/transcription/DualViewPanel.tsx` | 1187 |
| `apps/desktop/src-tauri/src/native_probe.rs` | 1182 |
| `apps/desktop/src-tauri/src/main.rs` | 1162 |
| `apps/desktop/src/features/transcription/hooks/useDualViewTranscript.ts` | 1081 |
| `core/src/session/publisher.rs` | 1017 |
| `apps/desktop/src-tauri/src/session.rs` | 978 |

> 其他超过 800 行的文件主要是 `Cargo.lock`/`package-lock.json` 及 `vendor/` 下的第三方源码或资源，列于文末。

## 详细拆分建议

### `core/src/orchestrator/mod.rs`
- **现状**：聚合引擎配置、`SpeechEngine`/`SentencePolisher` 定义、轻量润色器实现、会话调度器、实时会话句柄、通知/遥测处理、并内联平台特定实现和集成测试。功能横跨配置、执行、持久化、遥测。
- **问题**：类型定义与运行态逻辑混杂，`EngineOrchestrator` 的状态机难以单元测试；高耦合导致模块化扩展困难。
- **拆分策略**：
  1. 创建 `core/src/orchestrator/config.rs` 存放 `EngineConfig`、`RealtimeSessionConfig`，并暴露序列化逻辑。
  2. 新建 `core/src/orchestrator/traits.rs` 抽离 `SpeechEngine`、`SentencePolisher` trait 及 `LightweightSentencePolisher`，并将辅助函数私有化。
  3. 将会话生命周期与异步任务拆分为 `core/src/orchestrator/runtime/` 子目录（`mod.rs` 只 re-export），内含 `state.rs`（内部状态结构）、`tasks.rs`（音频消费、抛光、通知分发）、`handle.rs`（`RealtimeSessionHandle`）。
  4. 把通知/遥测写入迁移到 `telemetry.rs`，提供清晰接口以便复用与测试。
  5. 在根 `mod.rs` 仅组合公共 re-export，并保留 orchestrator 构造函数入口，确保文件 < 400 行。
- **实施步骤**：拆分类型 -> 重写 `mod.rs` 引用 -> 更新 `use` 路径 -> 补增单元测试覆盖各子模块。

### `apps/desktop/src/App.tsx`
- **现状**：单个组件承担全局状态管理、权限请求、热键探测、音频诊断、UI 布局、教程引导等职责，内联大量辅助函数和常量。
- **问题**：难以复用逻辑，`useEffect` 链条难维护；UI 与业务逻辑耦合严重。
- **拆分策略**：
  1. 抽出权限与教程逻辑为 `hooks/usePermissionStatus.ts` 与 `hooks/useTutorialProgress.ts`。
  2. 将 Fn/自定义热键探测流程移至 `hooks/useHotkeyProbe.ts`（封装定时器、音频波形、反馈状态）。
  3. 把音频诊断与仪表盘拆成 `components/AudioDiagnosticsPanel.tsx` 与 `components/AudioMeter.tsx`。
  4. 主 App 保留路由/布局渲染，使用拆分后的子组件组合；常量移动至 `constants/app.ts`。
- **实施步骤**：定位相关 `useState`/`useEffect`，迁移到新 Hook；子组件通过 props 接收状态/回调；整理 `invoke` 调用集中到 service 层。

### `core/src/session/mod.rs`
- **现状**：会话状态机、噪声/静默事件、持久化桥接、剪贴板降级、遥测记录、后台任务调度全部集中于一个模块。
- **问题**：事件类型与业务逻辑交错，广播处理难测试；对持久化的调用散乱。
- **拆分策略**：
  1. 保留顶层 `mod.rs` 仅声明子模块和对外 API。
  2. 按功能拆成 `events.rs`（噪声、静默结构体 + 枚举）、`state.rs`（核心会话状态与互斥锁）、`persistence.rs`（与 `PersistenceActor` 的交互）、`telemetry.rs`（记录函数）。
  3. 将后台任务（历史清理、自动停止计时）放入 `tasks.rs`，由 `SessionController` 统一驱动。
  4. 针对剪贴板和历史导出保留现有子模块，更新 `pub use`。
- **实施步骤**：先移动数据结构与常量，再迁移逻辑函数并修复路径，最后精简 `mod.rs`。

### `apps/desktop/src-tauri/src/audio.rs`
- **现状**：音频设备枚举、权限检查、噪声诊断、波形合成、校准与文件写入混在同一文件；包含 Windows 专用流程。
- **问题**：平台分支和业务流程交织，影响测试；难以替换音频后端。
- **拆分策略**：
  1. 新建 `audio/device.rs` 管理设备枚举、首选设备选择。
  2. 将权限/系统设置打开逻辑迁移到 `audio/permissions.rs`。
  3. 把诊断与噪声分析拆到 `audio/diagnostics.rs`，只对外暴露聚合结果。
  4. 将波形/录制写入封装在 `audio/waveform.rs`。
  5. 主 `audio/mod.rs` 仅组合公共 API，保证每个子模块 < 300 行。
- **实施步骤**：创建子模块，移动结构体与函数；更新 `main.rs` 中引用；补充单元测试验证拆分后接口。

### `apps/desktop/src-tauri/src/hotkey.rs`
- **现状**：管理热键配置、Fn 探测、HMAC 密钥加载、跨平台兼容层、音频反馈等。
- **问题**：配置、运行时和平台逻辑互相穿插；测试难以隔离。
- **拆分策略**：
  1. 建立 `hotkey/config.rs`（`HotkeyBinding`、`HotkeySource`、序列化）。
  2. 将探测与兼容层拆到 `hotkey/probe.rs` 和 `hotkey/compat.rs`。
  3. 保留 `AppState`/`CalibrationMode` 在 `hotkey/state.rs`，负责共享状态与 Tauri `State`。
  4. 音频反馈与 UI 提示抽到 `hotkey/feedback.rs`，导出可供前端消费的结构。
- **实施步骤**：迁移类型定义 -> 重写 `pub use` -> 更新 `main.rs` 调用点。

### `apps/desktop/src/features/transcription/DualViewPanel.tsx`
- **现状**：组件内定义国际化文案、布局、滚动管理、快捷键、批量操作、历史面板、噪声和静默提示渲染。
- **问题**：渲染逻辑与状态管理高度耦合；多列布局与操作按钮难以复用。
- **拆分策略**：
  1. 将文案与常量迁移到 `DualViewPanel.messages.ts`。
  2. 拆出 `TranscriptColumn`、`SentenceToolbar`、`ResultSummary`、`HistoryDrawer` 等子组件。
  3. 将键盘与滚动同步逻辑放入 `hooks/useDualViewKeyboard.ts` 与 `hooks/useSyncedScroll.ts`。
  4. 主组件仅负责组合与路由状态，目标行数 < 400。
- **实施步骤**：抽离文案 -> 创建子组件 -> 分离 Hook -> 更新测试覆盖。

### `apps/desktop/src-tauri/src/native_probe.rs`
- **现状**：同一文件内包含 macOS、Windows、Linux 平台特定实现与公共结构。
- **问题**：条件编译模块过长，平台互不相关却互相干扰；难以独立测试某个平台实现。
- **拆分策略**：
  1. 将公共类型保留在 `mod.rs`，创建 `platform/macos.rs`、`platform/windows.rs`、`platform/linux.rs`。
  2. 每个平台文件仅暴露 `run_probe()` 等最小接口。
  3. 主模块通过 `cfg` re-export 对应实现，并提供统一包装函数。
- **实施步骤**：移动 `cfg` 块至新文件 -> 更新 `Cargo.toml` 声明 -> 编写平台单元测试或模拟。

### `apps/desktop/src-tauri/src/main.rs`
- **现状**：集中定义所有 Tauri 命令、请求/响应结构体、状态初始化、窗口事件监听。
- **问题**：命令数多且参数复杂，文件过长；难以定位特定命令逻辑。
- **拆分策略**：
  1. 建立 `commands/` 目录（如 `commands/hotkey.rs`、`commands/audio.rs`、`commands/session.rs`），每个文件注册相关命令并返回响应类型。
  2. 公共响应结构移至 `types.rs`。
  3. 主 `main.rs` 仅负责应用启动、状态注入、命令列表注册。
- **实施步骤**：迁移结构体 -> 把 `tauri::Builder` 注册拆分函数 -> 更新前端调用路径。

### `apps/desktop/src/features/transcription/hooks/useDualViewTranscript.ts`
- **现状**：承担状态初始化、事件订阅、选择管理、发布流程、遥测上报、音频提示；暴露大量类型。
- **问题**：Hook 过于庞大，难以单测；上下文切换困难。
- **拆分策略**：
  1. 将类型定义与常量移动到 `types.ts`。
  2. 拆分成多个 Hook：`useTranscriptState`（状态与 reducer）、`useSelectionManager`、`usePublishingBridge`、`useNotices`。
  3. 创建 `context/DualViewProvider.tsx` 提供共享状态，主 Hook 组合子 Hook。
- **实施步骤**：识别共享状态 -> 创建 context -> 重写导出 API 并更新使用方。

### `core/src/session/publisher.rs`
- **现状**：包含配置、策略枚举、错误类型、发布器 trait 与多个实现（剪贴板、直接插入）、重试逻辑。
- **问题**：策略实现与公共接口紧密耦合；扩展新策略成本高。
- **拆分策略**：
  1. `publisher/mod.rs` 仅保留 trait 与公共类型 re-export。
  2. 创建 `publisher/config.rs`、`publisher/strategy.rs`（定义 `FallbackStrategy`）、`publisher/engine.rs`（`SessionPublisher` 具体实现）。
  3. 将平台特定插入代码放入 `platform/` 子模块。
- **实施步骤**：模块化类型 -> 拆分实现 -> 更新 `core/src/session/mod.rs` 引用。

### `apps/desktop/src-tauri/src/session.rs`
- **现状**：集成会话状态管理、发布通知、历史记录同步、事件广播、Tauri emit。
- **问题**：模型定义与异步桥接交织；`SessionStateManager` 难以复用。
- **拆分策略**：
  1. 拆出 `models.rs`（`SessionStatus`、`TranscriptSentence` 等数据结构）。
  2. `manager.rs` 专注于状态机与 emitter 交互。
  3. `publisher_bridge.rs` 处理与核心会话发布接口的交互、错误映射。
  4. 主 `mod.rs` 提供对外 API（启动、停止、订阅）。
- **实施步骤**：移动数据结构 -> 重写模块引用 -> 添加针对桥接的单元测试。

## 锁文件与第三方资源
- `apps/desktop/src-tauri/Cargo.lock`（5817 行）、`apps/desktop/package-lock.json`（3752 行）、`core/Cargo.lock`（1906 行）均为自动生成文件，无需手动拆分，建议通过依赖瘦身或启用 `npm prune` 控制体积。
- `vendor/` 目录下 80+ 个超过 800 行的第三方源码/头文件/资源（如 `whisper.cpp`、`asio`、`nlohmann/json.hpp`、`spdlog`）由上游维护。若需遵守 800 行限制，可考虑改为子模块引用或通过构建脚本下载，避免复制源码。

