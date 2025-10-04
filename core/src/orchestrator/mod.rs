//! 引擎编排服务脚手架。

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tracing::info;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineConfig {
    pub prefer_cloud: bool,
}

#[async_trait]
pub trait SpeechEngine: Send + Sync {
    async fn transcribe(&self, frame: &[f32]) -> Result<String>;
}

pub struct EngineOrchestrator {
    config: EngineConfig,
}

impl EngineOrchestrator {
    pub fn new(config: EngineConfig) -> Self {
        Self { config }
    }

    pub async fn warmup(&self) -> Result<()> {
        info!(
            target: "engine_orchestrator",
            prefer_cloud = self.config.prefer_cloud,
            "warming up engines"
        );
        Ok(())
    }
}
