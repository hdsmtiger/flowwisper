# Flowwisper 项目上下文

## 项目概述

Flowwisper 是一个语音输入助手项目，采用单体仓库（monorepo）结构，包含桌面端、核心服务以及云端组件。主要技术栈包括：

- **桌面端**：Tauri（Rust）+ React
- **核心服务**：Rust 守护进程
- **API 网关**：FastAPI (Python)
- **混合路由服务**：Go
- **管理后台**：Next.js (React)

## 项目结构

```
flowwisper/
├─ apps/
│  └─ desktop/                # Tauri + React 桌面应用
│     ├─ src/                 # React UI 与 hooks
│     └─ src-tauri/           # 桌面端 Rust 命令、打包配置
├─ core/                     # Rust 实现的后台守护进程
│  ├─ src/                    # 音频、会话、编排等子模块
│  └─ Cargo.toml
├─ services/
│  ├─ api_gateway/           # FastAPI API 网关
│  │  ├─ flowwisper_api/     # FastAPI 应用入口
│  │  └─ tests/              # API 端到端/单元测试
│  ├─ hybrid_router/         # Go 编写的混合引擎编排服务
│  └─ admin_console/         # Next.js 管理后台 UI
├─ docs/                     # 需求与架构文档
├─ infra/                    # 部署与基础设施脚本
└─ scripts/                  # build_all.sh 等通用脚本
```

## 构建与运行

### 一键构建脚本

仓库提供 `scripts/build_all.sh`，用于串行执行各子项目的依赖安装、编译与基础测试：

```bash
./scripts/build_all.sh
```

脚本默认：

1. 运行 `cargo test` 验证 `core` Rust 守护进程。
2. 对桌面端与管理后台执行 `npm install && npm run build`。
3. 对 Go Hybrid Router 执行 `go test ./...`。
4. 在 FastAPI Gateway 内创建虚拟环境，安装依赖并运行 `pytest` 冒烟测试。

### 各模块运行命令

**核心服务 (Rust 守护进程)**

- 测试：`cargo test --manifest-path core/Cargo.toml`
- 运行：(需要查看 core/src/main.rs 来确定具体命令)

**桌面端 (Tauri + React)**

- 安装依赖：`cd apps/desktop && npm install`
- 开发模式：`npm run dev`
- 构建：`npm run build`
- 桌面端 Rust 命令调试：`cargo run --manifest-path apps/desktop/src-tauri/Cargo.toml`

**API 网关 (FastAPI)**

- 安装依赖：`cd services/api_gateway && pip install -e .[dev]`
- 开发模式：`uvicorn flowwisper_api.main:app --reload`
- 测试：`pytest`

**混合路由服务 (Go)**

- 测试：`cd services/hybrid_router && go test ./...`
- 运行：`go run main.go`

**管理后台 (Next.js)**

- 安装依赖：`cd services/admin_console && npm install`
- 开发模式：`npm run dev`
- 构建：`npm run build`

## 开发规范

### 编码风格

- **Rust**：遵循 `cargo fmt` 格式化标准，文件名使用 snake_case，公共类型使用 PascalCase。
- **TypeScript (React)**：使用 2 空格缩进，React 组件使用 PascalCase，Hooks 使用 `use` 前缀。
- **Python**：遵循 PEP 8，使用 `pydantic` 进行配置验证。
- **Go**：遵循 `gofmt`/`goimports` 格式化标准。

### 测试规范

- 单元测试和集成测试与模块代码放在一起。
- Rust 测试放在 `core/src/**` 或 `apps/desktop/src-tauri/tests/`。
- FastAPI 测试放在 `services/api_gateway/tests/`。
- Go 测试文件使用 `_test.go` 后缀。
- 提交代码前运行对应模块的测试命令。