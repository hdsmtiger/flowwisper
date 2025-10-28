# `core/src/session/publisher` 拆分设计

## 现状概述
`core/src/session/publisher.rs` 集中定义了发布流程所需的全部结构：配置、上下文、错误、自动化能力接口、发布器实现以及大量测试，总计 1,017 行。类型定义与执行逻辑紧密耦合，导致：
- 新增插入策略或扩展自动化通道时，需要在同一文件中修改多处，阅读成本高。
- `Publisher` 行为难以按子模块编写单元测试或替换实现，因为 trait、错误类型、配置常量全部耦合在一个文件里。
- 会话状态机（`core/src/session/mod.rs`）只依赖其中的公共类型和 `SessionPublisher` trait，但必须导入整个文件。

## 依赖分析
- **上游调用方**：
  - `core/src/session/mod.rs` 中的 `SessionManager` 通过 `Arc<dyn SessionPublisher>` 调用发布流程，并直接使用 `FallbackStrategy`、`PublishOutcome`、`PublishRequest`、`PublisherFailure` 等类型记录遥测与 UI 事件。
  - 单元测试通过 `Publisher::default()` 以及构造 `PublishRequest` 验证重试和降级逻辑。
- **下游依赖**：
  - `Publisher` 内部依赖 `FocusAutomation` trait（默认实现为 `SystemFocusAutomation`），用于对接平台自动化通道。
  - `AutomationError` 被转换为 `PublisherFailure`、`PublisherError` 并在遥测记录中携带。
  - `tokio` 运行时在测试中被使用，允许我们保留异步接口。

## 拆分目标
- 将类型定义（配置、上下文、状态、失败原因等）迁移到独立的 `types` 模块，供其他模块复用。
- 将自动化通道与错误定义放入 `automation` 模块，聚合 `FocusAutomation` trait、`AutomationError` 以及系统默认实现。
- 将发布器核心逻辑迁移到 `engine` 模块，仅关注 `Publisher` 结构与 `SessionPublisher` trait，实现仍依赖 `types` 与 `automation`。
- 在 `mod.rs` 中集中 re-export 公共 API，维持 `core::session` 调用面不变。
- 将测试移动到 `tests.rs`，通过引用新模块验证功能。

## 重构步骤
1. 新建 `core/src/session/publisher/` 目录，创建 `mod.rs` 并声明子模块。
2. 按上述模块拆分原始文件内容，修复相对引用与 `use` 路径。
3. 更新 `core/src/session/mod.rs` 的导入路径，保持对外 API 不变。
4. 运行 `cargo fmt` 和 `cargo test -p core` 相关测试，确保行为一致。

## 额外注意事项
- 拆分后 `pub use` 的顺序与命名需保持兼容，以免破坏其他模块的依赖。
- `SystemFocusAutomation` 仍作为默认实现存在，但保持模块内部可见，通过 `Publisher::default()` 间接使用。
- 测试模块需要根据新的模块划分调整 `use` 路径，确保覆盖率不下降。
