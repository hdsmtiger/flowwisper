//! 引擎编排服务脚手架。

mod constants;
mod engine;
mod runtime;

pub mod config;
pub mod traits;
pub mod types;

pub use config::{EngineConfig, RealtimeSessionConfig};
pub use engine::EngineOrchestrator;
pub use runtime::RealtimeSessionHandle;
pub use traits::{SentencePolisher, SpeechEngine};
pub use types::{NoticeLevel, SessionNotice, TranscriptSource, TranscriptionUpdate, UpdatePayload};

#[cfg(test)]
mod tests;
