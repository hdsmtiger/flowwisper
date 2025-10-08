# 本地历史记录持久化 Runbook

本文档描述 Flowwisper 桌面端 UC2.4 历史记录能力的运维与排障步骤，涵盖密钥管理、数据库清理以及常见失败场景的处置方法。

## 环境准备

1. 确认桌面端和核心守护进程使用相同的数据目录（默认 `~/.config/Flowwisper` 或 `AppData/Roaming/Flowwisper`）。
2. 设置 SQLCipher 密钥：
   ```bash
   export FLOWWISPER_SQLCIPHER_KEY="<随机 64 字符十六进制>"
   ```
   - macOS 可通过 `security find-generic-password` 从钥匙串读取；
   - Windows 建议使用 DPAPI/TPM 加密后存储在 Credential Manager，再由启动脚本写入环境变量。
3. 可选：`RUST_LOG=persistence=trace,session_manager=info` 以便采集调试日志。

## 密钥轮换

1. 停止桌面端进程与核心守护进程。
2. 备份现有数据库（`history.db`）：
   ```bash
   cp ~/.config/Flowwisper/history.db ~/.config/Flowwisper/history.db.bak
   ```
3. 生成新密钥并写入 `FLOWWISPER_SQLCIPHER_KEY`。
4. 执行离线重新加密：
   ```bash
   sqlcipher ~/.config/Flowwisper/history.db <<'SQL'
   pragma key = '旧密钥';
   pragma rekey = '新密钥';
   .quit
   SQL
   ```
   - 若 sqlcipher 不可用，可使用 Flowwisper 提供的 `scripts/rekey_history.sh`（待发布）自动化执行。
5. 重新启动应用，运行 `tauri invoke session_history_search '{}'` 验证读取是否成功。
6. 删除备份文件或将其安全存档。

## 数据库重置

用于 QA 或清除损坏数据库。

```bash
rm ~/.config/Flowwisper/history.db
```

- 重新启动桌面端或核心守护进程时会自动重建 schema；
- 首次调用任意历史接口前，`SqlitePersistence::bootstrap` 会验证文件存在并运行迁移。

## TTL 清理验证

默认每 30 分钟自动清理一次。若需加速验证：

1. 使用 sqlcipher 将任意测试会话的 `completed_at_ms` 调整为过去时间：
   ```sql
   UPDATE sessions SET completed_at_ms = 0, expires_at_ms = 1 WHERE session_id = 'test';
   ```
2. 启动核心守护进程并设置 `RUST_LOG=persistence=trace`，观察 30 分钟内是否出现 `session_history_cleanup` 事件；
3. 或直接运行单元测试：
   ```bash
   cargo test --manifest-path core/Cargo.toml cleanup_expired_removes_sessions -- --nocapture
   ```
   该测试会模拟过期记录并验证 `sessions` 与 `session_index` 均被清理。

## 常见故障与排查

| 症状 | 可能原因 | 处理步骤 |
| --- | --- | --- |
| `history_persist_failure` 遥测持续出现，UI 提示“历史记录保存失败” | SQLCipher 密钥缺失或错误；数据库文件权限不足；磁盘已满 | 检查环境变量、运行 `sqlcipher history.db "pragma key='<密钥>'; pragma cipher_integrity_check;"`；确认数据目录可写并释放磁盘空间 |
| Tauri `session_history_*` 命令返回 `unable to open database file` | 桌面端与核心进程使用不同数据目录；进程无权限创建目录 | 设置 `FLOWWISPER_DATA_DIR` 为共享路径并确保目录存在；在 macOS 授权“完全磁盘访问” |
| 搜索结果为空或缺少预期条目 | FTS 索引未同步或 TTL 过期 | 在日志中确认 `session_index` 触发器执行；手动运行清理命令后重试；检查条目 `expires_at_ms` 是否小于当前时间 |
| 准确性标记无法保存 | `update_accuracy` 命令未成功提交或缓存未刷新 | 检查 `session_history_mark_accuracy` 响应；确认前端调用 `clearHistoryCache()`；查看日志中的 `session_history_accuracy` 事件 |
| 剪贴板备份提示频繁出现 | 持久化多次失败导致回退 | 使用 `RUST_LOG=persistence=trace` 查看失败原因；确认磁盘写入性能；必要时临时禁用 SQLCipher（移除密钥）定位问题 |

若需扩展 runbook，请在 PR 中同步更新 [Sprint 文档](../sprint/sprint2.md)。
