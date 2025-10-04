//! 本地持久化层脚手架。

use anyhow::Result;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tracing::info;

#[derive(Debug, Serialize, Deserialize)]
pub struct TranscriptRecord {
    pub session_id: String,
    pub raw_text: String,
    pub polished_text: String,
}

pub struct PersistenceActor {
    rx: mpsc::Receiver<TranscriptRecord>,
}

impl PersistenceActor {
    pub fn new(rx: mpsc::Receiver<TranscriptRecord>) -> Self {
        Self { rx }
    }

    pub async fn run(mut self) -> Result<()> {
        while let Some(record) = self.rx.recv().await {
            info!(target: "persistence", ?record, "received transcript record");
            // TODO: 将记录写入 SQLCipher + FTS5。
        }
        Ok(())
    }
}
