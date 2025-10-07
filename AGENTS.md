# Repository Guidelines

## Project Structure & Module Organization
The monorepo groups client, core, and service layers. `apps/desktop/` contains the Tauri + React shell (`src/` for UI, `src-tauri/` for Rust commands). `core/` hosts the long-lived Rust daemon with domain modules under `src/` (e.g., `audio/`, `session/`, `telemetry/`). Cloud-facing services live in `services/`: `api_gateway/` (FastAPI package inside `flowwisper_api/`), `hybrid_router/` (Go entrypoint `main.go`), and `admin_console/` (Next.js UI). Shared scripts sit in `scripts/`, and product docs remain in `docs/`.

## Build, Test, and Development Commands
Use `./scripts/build_all.sh` for a full pipeline run; set `RUN_TAURI_BUNDLE=1` to request a native desktop bundle. Work iteratively per module: `cargo run` (or `cargo test`) in `core/`, `npm run dev` / `npm run build` in `apps/desktop`, `npm run dev` / `npm run build` in `services/admin_console`, `go run main.go` in `services/hybrid_router`, and `uvicorn flowwisper_api.main:app --reload` inside `services/api_gateway` after `pip install -e .[dev]`.

## Coding Style & Naming Conventions
Adopt idiomatic language defaults. For Rust modules (`core/`, `apps/desktop/src-tauri/`), run `cargo fmt --all` and keep files snake_case with public types in PascalCase. Front-end TypeScript favors two-space indentation, PascalCase React components, and `use`-prefixed hooks; colocate component styles within `apps/desktop/src/`. For Next.js, follow the same conventions and keep route folders kebab-case. Python code in `flowwisper_api/` should satisfy PEP 8; prefer dataclass-style settings objects and validate configs with `pydantic`. Go packages in `services/hybrid_router/` must pass `gofmt`/`goimports`; expose only deliberate interfaces. 将单个源文件控制在 800 行以内，必要时抽取子模块或组件。

## Testing Guidelines
Keep unit and integration tests alongside their modules: Rust tests under `core/src/**` or `apps/desktop/src-tauri/tests/`, FastAPI tests in `services/api_gateway/tests/`, and Go specs next to packages. Use descriptive `mod tests` names in Rust, `test_*` functions in Python, and `_test.go` suffixes in Go. Run `cargo test`, `npm run test:backend` (desktop Tauri layer), `pytest`, and `go test ./...` before submitting. Capture new behaviors with focused fixtures rather than overloading smoke tests.

## Commit & Pull Request Guidelines
Follow the existing history pattern of imperative messages with optional scopes (`docs:`, `chore:`, `feat:`). Group logical changes per commit and reference issue IDs when available. PRs should summarize impact, list test commands executed, and include screenshots or CLI logs for UI or API surface changes. Flag breaking workflow changes and document any new environment variables in the description.

## Task Implemention Sequence
You ALWAYS follow instructions in .ai-dev-tasks/process-task-list.md to implement tasks. If you forgot the instructions, read the file again.
