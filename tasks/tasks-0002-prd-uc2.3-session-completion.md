## Relevant Files

- `apps/desktop/src/features/transcription/DualViewPanel.tsx` - 承载转写界面，并在会话完成时展示结果卡片与操作状态。
- `apps/desktop/src/features/transcription/hooks/useDualViewTranscript.ts` - 负责订阅会话状态/事件，需要扩展发布阶段、撤销提示、插入反馈。
- `apps/desktop/src/features/transcription/test` - 集成与可访问性测试目录，验证结果卡片与快捷键行为。
- `apps/desktop/src-tauri/src/session.rs` - Tauri 侧会话状态桥接，需新增 Publishing 状态与结果事件下发。
- `apps/desktop/src-tauri/src/lib.rs` - 注册新的 Tauri 命令（插入、复制、草稿保存、通知中心写入）。
- `core/src/session/mod.rs` - 核心会话状态机与 orchestrator，需串联 Publishing 阶段、插入尝试、降级策略。
- `core/src/session`（新建 `publisher.rs`、`clipboard.rs` 等） - 封装光标插入、剪贴板备份/恢复、失败重试逻辑。
- `core/src/persistence/mod.rs` - 本地加密 SQLite 接口，扩展片段草稿与通知中心写入。
- `core/src/telemetry/events.rs` - 记录插入/复制成功率、失败原因、重试次数的遥测事件。
- `services`（如需） - 若通知中心或历史记录有服务端回落，需要确认是否同步 API。

### Notes

- 默认策略是“自动插入”，需保持 99% 直接插入成功率，并在失败时自动复制且提示用户降级路径。
- 所有用户可见状态（包括插入进度、失败原因、撤销提示）必须可通过键盘与屏幕阅读器访问。
- 剪贴板降级时要保留原内容备份，撤销或失败后自动恢复，确保不破坏用户剪贴板历史。
- 片段草稿保存与通知中心记录要使用 SQLCipher 持久化，并考虑异步写入避免阻塞 UI。

## Tasks

- [ ] 1.0 扩展核心会话状态机以支持 Publishing 阶段与插入结果回传
  - [ ] 1.1 在 `SessionManager` 中定义 Publishing/Completed/Failed 状态与事件负载，确保与 Tauri 桥接契约一致。
  - [ ] 1.2 新增 `publisher.rs`（或同等模块）封装插入流程入口，接受润色稿、焦点窗口上下文、回退策略。
  - [ ] 1.3 将 SessionManager 的会话结束流程串联 publisher，记录插入尝试、成功/失败、降级动作并发送遥测。

- [ ] 2.0 实现跨平台插入与剪贴板降级逻辑
  - [ ] 2.1 在 publisher 模块中调用 macOS Accessibility API / Windows UI Automation 检测焦点输入框可写性，并在 400ms 内执行粘贴或模拟键入。
  - [ ] 2.2 构建 `clipboard.rs` 管理剪贴板备份、写入、恢复，支持纯文本格式并暴露降级接口。
  - [ ] 2.3 设计失败判定（超时、拒绝、无焦点）与重试策略，超过两次失败返回明确错误码供 UI 展示。
  - [ ] 2.4 将降级自动复制流程与遥测、通知中心记录打通，确保 200ms 内完成备份与复制。

- [ ] 3.0 扩展 Tauri 桥接以传递完成卡片状态与操作
  - [ ] 3.1 在 `src-tauri` 注册命令触发插入、复制、草稿保存，并向前端发送进度/结果事件（含重试、撤销提示）。
  - [ ] 3.2 追加事件类型到 `session.rs`（如 `PublishingUpdate`、`InsertionResult`），并维护最多 120 条历史以支持重放与调试。
  - [ ] 3.3 为通知中心写入、撤销提示等提供命令或事件通道，确保 UI 能响应状态变化。

- [ ] 4.0 构建前端结果卡片 UI 与交互
  - [ ] 4.1 在 `DualViewPanel` 或新组件中渲染结果卡片，展示润色稿文本、操作按钮、进度/失败状态、撤销提示。
  - [ ] 4.2 更新 `useDualViewTranscript` 以订阅 Publishing 事件、处理自动插入完成、降级、重试、撤销、键盘导航与屏幕阅读器朗读。
  - [ ] 4.3 添加国际化文案与 0.5 秒提示 SLA 的动画/提示条，实现 3 秒后自动淡出并保留历史记录入口。
  - [ ] 4.4 覆盖可访问性与交互测试（快捷键、按钮状态、失败重试流程）并更新现有单元/集成测试。

- [ ] 5.0 片段草稿与通知中心持久化
  - [ ] 5.1 在 Persistence 层新增片段草稿表结构与保存 API，支持标题/标签默认值与异步写入。
  - [ ] 5.2 实现通知中心记录写入接口（动作类型、结果、时间戳），并暴露查询命令供 UI 历史查看。
  - [ ] 5.3 将保存草稿命令与 Core 服务打通，处理成功/失败反馈与重试日志导出能力。

- [ ] 6.0 测试与监控覆盖
  - [ ] 6.1 为插入/剪贴板模块编写 Rust 单元测试与集成测试（模拟成功、失败、超时、降级）。
  - [ ] 6.2 为结果卡片与通知中心新增前端测试，验证状态流与辅助功能。
  - [ ] 6.3 定义遥测指标（插入成功率、降级率、撤销触发次数），并在文档或仪表盘配置中记录采集方式。
