# Flowwisper Monorepo

本仓库包含 Flowwisper Fn 语音输入助手的多模块脚手架，覆盖桌面端、核心服务以及云端组件，便于按照《PRD》和《架构设计》推进后续开发。贡献流程与协作规范请参阅 [Repository Guidelines](AGENTS.md)。

## 目录结构

- `apps/desktop/`：Tauri + React 桌面壳层脚手架。
- `core/`：Rust 实现的后台守护进程骨架。
- `services/api_gateway/`：FastAPI API 网关。
- `services/hybrid_router/`：Go 编写的混合引擎编排服务。
- `services/admin_console/`：Next.js 管理后台 UI。
- `docs/`：需求与架构文档。
- `infra/`：部署与基础设施脚本。
- `scripts/`：跨模块自动化脚本。

参考下方概要快速定位目录：

```
flowwisper/
├─ apps/
│  └─ desktop/
│     ├─ src/                 # React UI 与 hooks
│     └─ src-tauri/           # 桌面端 Rust 命令、打包配置
├─ core/
│  ├─ src/                    # 音频、会话、编排等子模块
│  └─ Cargo.toml
├─ services/
│  ├─ api_gateway/
│  │  ├─ flowwisper_api/      # FastAPI 应用入口
│  │  └─ tests/               # API 端到端/单元测试
│  ├─ hybrid_router/          # Go 服务入口 main.go
│  └─ admin_console/          # Next.js 管理后台
├─ docs/                      # PRD、架构与冲刺记录
├─ infra/                     # IaC、部署脚本
└─ scripts/                   # build_all.sh 等通用脚本
```

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

## 构建与打包

### 桌面端开发构建

1. 安装依赖：
   ```bash
   cd apps/desktop
   npm install
   ```
2. 启动前端调试：`npm run dev`（Vite：默认 http://localhost:5173）。
3. Rust Tauri 命令位于 `apps/desktop/src-tauri/`，可在单独终端执行 `cargo run --manifest-path src-tauri/Cargo.toml` 进行调试。

### Windows 安装包打包

先确保安装以下工具：
- Visual Studio Build Tools（含“使用 C++ 的桌面开发”组件）。
- `rustup target add x86_64-pc-windows-msvc`。
- Node.js ≥ 18 与 npm。

打包步骤：
```bash
cd apps/desktop
npm install                      # 首次或依赖更新时执行
npm run build                    # 进行前端与 Tauri 后端编译、自测
npx tauri build --target x86_64-pc-windows-msvc
```
生成的 `.msi` 安装包会输出到 `apps/desktop/src-tauri/target/release/bundle/msi/`。如果希望在一键脚本中同时生成安装包，可在仓库根目录执行 `RUN_TAURI_BUNDLE=1 ./scripts/build_all.sh`。

### macOS 安装包打包

确保已安装 Xcode Command Line Tools：`xcode-select --install`。若需通用包，可额外安装 `rustup target add aarch64-apple-darwin x86_64-apple-darwin`。

打包步骤：
```bash
cd apps/desktop
npm install
npm run build
npx tauri build                  # 默认输出当前 CPU 架构
# 若需通用包，可执行：
npx tauri build --target universal-apple-darwin
```
打包完成后，会在 `apps/desktop/src-tauri/target/release/bundle/dmg/` 下生成 `.dmg` 安装包，同时输出 `.app` 于 `bundle/macos/`。

## 下一步

1. 为桌面端补充会话 HUD、设备设置与 Waveform Telemetry Bridge。
2. 在 Core Service 集成音频驱动、引擎调用与同步逻辑。
3. 为 API Gateway/Hybrid Router 建立鉴权、指标与混合引擎策略。
4. 在 Admin Console 对接租户策略、审计日志与版本发布流程。
