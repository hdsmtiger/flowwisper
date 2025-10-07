# 脚手架代码审查与优化建议

本次审查针对仓库中已提交的多语言脚手架，重点关注可维护性、可构建性以及后续扩展空间。以下建议按照模块分类给出，可作为下一轮重构与实现的参考。

## 根仓库与协作流程
- **缺少统一的构建编排**：建议新增顶层构建脚本或自动化流程，统一驱动 Rust、Node.js、Go、Python 子项目的编译与测试，避免手工进入各目录执行命令。
- **开发环境约束未固化**：可以补充 `.tool-versions` 或 `mise`/`asdf` 配置，显式记录 Node、Rust、Go、Python 的版本范围，减少不同环境构建差异。
- **文档与代码联动**：README 中的“下一步”指引可链接到各模块的具体任务列表（Issue/Project），帮助团队成员快速认领任务。

## 桌面端（apps/desktop）
- **Tauri 会话状态**：目前仅在启动时通过 `invoke` 查询一次状态，建议使用 Tauri 事件（`emit`/`listen`）或建立轮询机制以响应核心服务状态变化。
- **状态共享锁粒度**：`Arc<Mutex<SessionStatus>>` 容易在 UI 与后台交互频繁时出现争用，可改为 `RwLock` 或基于 `State` 的订阅模型，以提升并发读性能。
- **打包流程**：`package.json` 尚未提供 Tauri 打包脚本，也缺少 `tauri.conf.json` 中针对多平台的打包配置（图标、签名、权限）。建议补充 `npm run tauri:dev/build` 等命令并完善打包元数据。

## 核心服务（core）
- **错误处理与日志**：模块函数普遍返回 `Result<()>` 但缺乏错误上下文。后续可引入 `tracing` 的 error 级别日志或 `anyhow::Context` 提供更多排障信息。
- **异步任务生命周期**：`PersistenceActor::run()` 在 `SessionManager::new()` 中被 `tokio::spawn`，但缺乏关闭信号，建议引入 `Shutdown` 机制或 `JoinHandle` 保存，避免程序退出时资源泄露。
- **配置抽象**：`EngineConfig` 目前硬编码 `prefer_cloud = true`，可引入 `serde` + `config` 读取外部配置，方便在不同环境切换策略。

## API Gateway（services/api_gateway）
- **PyProject 构建系统缺失**：`pyproject.toml` 未声明 `build-system`，导致 `pip install .` 失败。需补充构建后端配置并提供最小化的测试用例以保障 CI。
- **运行配置**：建议使用 `pydantic-settings` 的模型校验 `.env` 配置，并提供示例 `.env.example`，确保配置项明确。
- **测试覆盖**：目前缺少自动化测试，可以先添加 FastAPI 应用的 smoke test，验证路由是否可加载。

## Hybrid Router（services/hybrid_router）
- **模块化拆分**：所有逻辑写在 `main.go` 中，后续应拆分为 handler、router、client 等包，便于编写单元测试。
- **配置与依赖注入**：建议引入 `cobra` 或 `urfave/cli` 管理命令行参数，方便扩展运行模式，并通过接口注入 mock 以支持测试。
- **监控与日志**：可以集成 `zap`/`logrus` 等结构化日志库，以及 Prometheus 指标暴露，为混合编排策略调优做准备。

## Admin Console（services/admin_console）
- **UI 状态管理**：当前页面仅为静态占位，建议定义全局状态（如使用 Zustand/Redux），并拆分组件，方便后续接入 API。
- **类型定义**：可添加 `types/` 目录维护接口响应的 TypeScript 类型，与后端契约保持一致。
- **构建性能**：默认 Next.js 配置未启用 `swcMinify`、`experimental.appDir` 等选项，后续根据需求优化打包体积。

> 以上建议可结合即将引入的自动化构建脚本及 CI 流程逐步落地。

## 2024-UC2.2 双视图落地审查
- **句级回退命令缺失（阻断核心交互）**：前端钩子 `useDualViewTranscript` 通过 `session_transcript_apply_selection` Tauri 命令下发句级回退请求，但桌面端 `invoke_handler` 未暴露该命令，因此所有回退操作都会失败。这直接违背 UC2.2 “允许句级切换/恢复原始稿”的验收要求，需要在 `src-tauri` 层补齐对应命令及管线对接。
  - ✅ 桌面端现已注册 `session_transcript_apply_selection` 命令，并通过 `SessionStateManager::apply_transcript_selection` 立即回放确认事件，满足句级回退闭环。
- **润色稿仍为原文回显**：核心编排默认注入的是 `IdentitySentencePolisher`，仅对字符串做 `trim` 后原样返回，导致润色视图与原始视图内容一致，无法满足 PRD 中“右侧 2.5s 内呈现 AI 润色稿”的硬性要求。需接入真实的润色引擎或至少引入可配置实现。
  - ✅ 默认多视图已切换为 `LightweightSentencePolisher`，支持填充去噪、口语化语法修正与自动补标点，确保润色列与原始列差异化呈现。
- **原始稿增量刷新超过 200ms**：`RealtimeSessionConfig` 把 `raw_emit_window` 设定为 400ms，上游即便以 100-200ms 帧率推送，只要没有句号边界就会延迟到 400ms 才落地文本，不符合 PRD 对“每 200ms 推送增量字符”的要求。建议将窗口收紧至 ≤200ms，并在句缓冲策略上对齐需求文档。
  - ✅ `RealtimeSessionConfig` 默认 `raw_emit_window` 已收紧到 200ms，配合句缓冲在无标点场景下也会按 200ms 窗口强制刷新原始稿。
