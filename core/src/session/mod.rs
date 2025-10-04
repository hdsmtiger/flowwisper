//! 会话管理状态机脚手架。

use crate::audio::AudioPipeline;
use crate::orchestrator::{EngineConfig, EngineOrchestrator};
use crate::persistence::{PersistenceActor, TranscriptRecord};
use anyhow::Result;
use tokio::sync::mpsc;
use tracing::info;

pub struct SessionManager {
    audio: AudioPipeline,
    orchestrator: EngineOrchestrator,
    persistence_tx: mpsc::Sender<TranscriptRecord>,
}

impl SessionManager {
    pub fn new() -> Self {
        let audio = AudioPipeline::new();
        let orchestrator = EngineOrchestrator::new(EngineConfig { prefer_cloud: true });
        let (persistence_tx, persistence_rx) = mpsc::channel(32);

        tokio::spawn(PersistenceActor::new(persistence_rx).run());

        Self {
            audio,
            orchestrator,
            persistence_tx,
        }
    }

    pub async fn run(&self) -> Result<()> {
        info!(target: "session_manager", "running bootstrap tasks");
        self.audio.start().await?;
        self.orchestrator.warmup().await?;
        Ok(())
    }

    pub async fn publish_transcript(&self, record: TranscriptRecord) -> Result<()> {
        self.persistence_tx
            .send(record)
            .await
            .map_err(|err| anyhow::anyhow!("failed to send transcript: {err}"))
    }
}
