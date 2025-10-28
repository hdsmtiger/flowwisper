use std::sync::Arc;
use std::time::Duration;

use crate::orchestrator::config::RealtimeSessionConfig;
use crate::orchestrator::types::{SentenceSelection, TranscriptCommand, TranscriptionUpdate};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::warn;

use super::state::LocalProgress;

pub struct RealtimeSessionHandle {
    pub(crate) config: RealtimeSessionConfig,
    pub(crate) frame_tx: mpsc::Sender<Arc<[f32]>>,
    pub(crate) command_tx: mpsc::Sender<TranscriptCommand>,
    updates_tx: mpsc::Sender<TranscriptionUpdate>,
    local_progress: Arc<LocalProgress>,
    monitor: Option<JoinHandle<()>>,
    worker: Option<JoinHandle<()>>,
}

impl RealtimeSessionHandle {
    pub(super) fn new(
        config: RealtimeSessionConfig,
        frame_tx: mpsc::Sender<Arc<[f32]>>,
        command_tx: mpsc::Sender<TranscriptCommand>,
        updates_tx: mpsc::Sender<TranscriptionUpdate>,
        local_progress: Arc<LocalProgress>,
        monitor: JoinHandle<()>,
        worker: JoinHandle<()>,
    ) -> Self {
        Self {
            config,
            frame_tx,
            command_tx,
            updates_tx,
            local_progress,
            monitor: Some(monitor),
            worker: Some(worker),
        }
    }

    pub async fn push_frame(
        &self,
        frame: Vec<f32>,
    ) -> Result<(), mpsc::error::SendError<Arc<[f32]>>> {
        if frame.is_empty() {
            warn!(target: "engine_orchestrator", "received empty audio frame");
            return Ok(());
        }

        let frame_duration =
            Duration::from_secs_f64(frame.len() as f64 / self.config.sample_rate_hz as f64);

        if frame_duration < self.config.min_frame_duration
            || frame_duration > self.config.max_frame_duration
        {
            warn!(
                target: "engine_orchestrator",
                ?frame_duration,
                min = ?self.config.min_frame_duration,
                max = ?self.config.max_frame_duration,
                "audio frame duration out of expected bounds"
            );
        }

        let shared: Arc<[f32]> = frame.into();
        match self.frame_tx.send(shared).await {
            Ok(()) => Ok(()),
            Err(err) => {
                warn!(
                    target: "engine_orchestrator",
                    %err,
                    "failed to enqueue audio frame"
                );
                Err(err)
            }
        }
    }

    pub fn frame_sender(&self) -> mpsc::Sender<Arc<[f32]>> {
        self.frame_tx.clone()
    }

    pub async fn apply_sentence_selections(
        &self,
        selections: Vec<SentenceSelection>,
    ) -> Result<(), mpsc::error::SendError<TranscriptCommand>> {
        if selections.is_empty() {
            return Ok(());
        }

        self.command_tx
            .send(TranscriptCommand::ApplySelection(selections))
            .await
    }

    #[cfg(test)]
    pub(crate) fn updates_sender(&self) -> mpsc::Sender<TranscriptionUpdate> {
        self.updates_tx.clone()
    }

    #[cfg(test)]
    pub(crate) fn local_progress(&self) -> Arc<LocalProgress> {
        Arc::clone(&self.local_progress)
    }
}

impl Drop for RealtimeSessionHandle {
    fn drop(&mut self) {
        let _ = self.updates_tx.is_closed();
        let _ = Arc::strong_count(&self.local_progress);
        if let Some(handle) = self.monitor.take() {
            handle.abort();
        }
        if let Some(worker) = self.worker.take() {
            worker.abort();
        }
    }
}
