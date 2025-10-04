# Flowwisper Monorepo

本仓库包含 Flowwisper Fn 语音输入助手的多模块脚手架，覆盖桌面端、核心服务以及云端组件，便于按照《PRD》和《架构设计》推进后续开发。

## 目录结构

- `apps/desktop/`：Tauri + React 桌面壳层脚手架。
- `core/`：Rust 实现的后台守护进程骨架。
- `services/api_gateway/`：FastAPI API 网关。
- `services/hybrid_router/`：Go 编写的混合引擎编排服务。
- `services/admin_console/`：Next.js 管理后台 UI。
- `docs/`：需求与架构文档。

## 开发准备

各子工程 README 中包含独立的本地运行指引。推荐使用 `mise` 或 `asdf` 管理多语言运行时，统一在根目录创建 `.tool-versions`（后续任务）。

### 一键构建脚本

仓库提供 `scripts/build_all.sh`，用于串行执行各子项目的依赖安装、编译与基础测试：

```bash
./scripts/build_all.sh
```

脚本默认：

1. 运行 `cargo test` 验证 `core` Rust 守护进程。
2. 对桌面端与管理后台执行 `npm install && npm run build`，如检测到 Tauri CLI 会尝试生成无 Bundle 的原生构建。
3. 对 Go Hybrid Router 执行 `go test ./...`。
4. 在 FastAPI Gateway 内创建虚拟环境，安装依赖并运行 `pytest` 冒烟测试。

## 下一步

1. 为桌面端补充会话 HUD、设备设置与 Waveform Telemetry Bridge。
2. 在 Core Service 集成音频驱动、引擎调用与同步逻辑。
3. 为 API Gateway/Hybrid Router 建立鉴权、指标与混合引擎策略。
4. 在 Admin Console 对接租户策略、审计日志与版本发布流程。
