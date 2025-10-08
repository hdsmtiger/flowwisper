# Sprint 2：实时转写与基础输出

## 目标
交付可在桌面端完成端到端本地语音输入的核心闭环，包括实时转写、双视图展示、会话结束控制以及基础历史管理。

## User Cases

1. **UC2.1 本地实时转写首字反馈**  
   - 描述：长按 Fn 进入录音后，Whisper 本地引擎在 400ms 内输出首批原始转写字符并持续 200ms 增量更新。  
   - 验收要点：音频帧以 100-200ms 粒度送入解码；首批文本延迟 ≤0.4s；异常时记录日志并回退提示。  
   - 需求来源：PRD 5.2、8.2；架构 5.2、7。

2. **UC2.2 原始稿与润色稿双视图展示**  
   - 描述：浮层左侧显示原始转写，右侧在 2.5 秒内呈现 AI 润色稿，并允许句级切换。  
   - 验收要点：UI 按 0.5s 延迟刷新润色稿；提供原始/润色切换按钮；用户可选择单句恢复原始稿。  
   - 需求来源：PRD 5.2、8.2；架构 5.1、6.1。

3. **UC2.3 会话结束与插入目标执行**  
   - 描述：用户按 Fn 或静默结束录音后，系统展示结果卡片并支持光标插入、复制剪贴板或保存为片段草稿。  
   - 验收要点：默认完成后在光标位置粘贴润色稿并提示撤销；插入失败提供重试；剪贴板/片段保存成功在通知中心记录。  
   - 需求来源：PRD 5.2、8.3；架构 6.1。

4. **UC2.4 历史记录本地存储与检索**
   - 描述：完成的会话写入 SQLCipher 加密 SQLite，48 小时内保留并支持关键词/应用过滤与分页检索；桌面端和核心守护进程均暴露接口供 UI 与测试消费。
   - 验收要点：
     1. 核心服务通过 `PersistenceActor` 在 200ms SLA、最多 3 次重试内落库，失败时触发剪贴板备份与遥测 `history_persist_failure`；
     2. `session_history_search`/`session_history_entry`/`session_history_mark_accuracy`/`session_history_append_action` Tauri 命令能够在无密钥与有密钥两种模式下工作，前端 Vitest 覆盖缓存命中与更新场景；
     3. Session Manager 仅调度一次 48 小时 TTL 清理任务，清理数量写入 `session_history_cleanup` 遥测并保持 FTS5 索引与主表一致；
     4. QA 按 [本地历史 runbook](../onboarding/local_history_runbook.md) 验证密钥轮换、数据库重置与常见报错排查。
   - 需求来源：PRD 5.2、8.3；架构 5.2、5.5、6.2。
   - 交付状态：核心守护进程、Tauri 命令与 TypeScript 客户端均已合并；`cargo test --manifest-path core/Cargo.toml` 与 `CI=1 npm test -- --run` 通过。

5. **UC2.5 噪音检测与自动结束**
   - 描述：录音过程中检测环境噪音突增或长时间静默，触发强降噪提示或自动结束会话。
   - 验收要点：噪音超过阈值时浮层弹出提示；静默超过配置阈值自动结束并写入状态；用户可手动恢复录音。
   - 需求来源：PRD 5.2、8.1；架构 5.2、6.1。

## QA Checklist：UC2.4 本地历史

| 检查项 | 预期结果 | 验证方式 |
| --- | --- | --- |
| 写入延迟 | 每次 `persist_session` 在 200ms SLA 内完成；重试不超过 3 次 | 运行 `RUST_LOG=persistence=trace cargo test --manifest-path core/Cargo.toml persistence::legacy_tests::saves_draft_with_defaults_and_retrieves_history -- --nocapture`，或在手动会话发布时观察日志中的 `session_history_persisted` 延迟字段 |
| 搜索 SLA | 关键字检索在 50ms 内返回 20 条以内结果 | 在桌面版启动后执行 `tauri invoke session_history_search '{"keyword":"demo"}'` 并在日志中确认 `history search completed` 耗时 |
| TTL 覆盖 | 超过 48 小时记录与索引条目被删除，并记入 `session_history_cleanup` 遥测 | 手动将 `completed_at_ms` 修改为过期时间后运行 `flowwisper-core`，等待半小时定时任务或调用 `tauri invoke session_history_search` 触发；查看 `~/.config/Flowwisper/history.db` 中 `sessions` 与 `session_index` 记录已清空 |
| 密钥轮换 | 更新 `FLOWWISPER_SQLCIPHER_KEY` 后数据库重新加密且可正常查询 | 参考 [runbook](../onboarding/local_history_runbook.md#%E5%AF%86%E9%92%A5%E8%BD%AE%E6%8D%A2) 执行步骤 |
| 故障回退 | 模拟写入失败后剪贴板包含润色稿备份，遥测出现 `history_persist_failure` | 将 `FLOWWISPER_SQLCIPHER_KEY` 设置为错误值并运行桌面端发布，观察前端提示与日志 |
