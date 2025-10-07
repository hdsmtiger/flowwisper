//! 会话管理状态机脚手架。

use crate::audio::AudioPipeline;
use crate::orchestrator::{
    EngineConfig, EngineOrchestrator, NoticeLevel, RealtimeSessionConfig, RealtimeSessionHandle,
    SessionNotice, TranscriptionUpdate, UpdatePayload,
};
use crate::persistence::{PersistenceActor, TranscriptRecord};
use anyhow::Result;
use tokio::sync::{broadcast, mpsc};
use tokio::time::{timeout, Duration};
use tracing::{info, warn};

pub struct SessionManager {
    audio: AudioPipeline,
    orchestrator: EngineOrchestrator,
    persistence_tx: mpsc::Sender<TranscriptRecord>,
    update_tx: broadcast::Sender<TranscriptionUpdate>,
}

impl SessionManager {
    pub fn new() -> Result<Self> {
        let audio = AudioPipeline::new();
        let orchestrator = EngineOrchestrator::new(EngineConfig {
            prefer_cloud: false,
        })?;
        Ok(Self::from_parts(audio, orchestrator))
    }

    pub fn with_orchestrator(orchestrator: EngineOrchestrator) -> Self {
        let audio = AudioPipeline::new();
        Self::from_parts(audio, orchestrator)
    }

    fn from_parts(audio: AudioPipeline, orchestrator: EngineOrchestrator) -> Self {
        let (persistence_tx, persistence_rx) = mpsc::channel(32);
        let (update_tx, _) = broadcast::channel(64);

        tokio::spawn(PersistenceActor::new(persistence_rx).run());

        Self {
            audio,
            orchestrator,
            persistence_tx,
            update_tx,
        }
    }

    pub async fn run(&self) -> Result<()> {
        info!(target: "session_manager", "running bootstrap tasks");
        self.audio.start().await?;
        self.orchestrator.warmup().await?;
        Ok(())
    }

    pub fn audio_pipeline(&self) -> AudioPipeline {
        self.audio.clone()
    }

    pub fn subscribe_updates(&self) -> broadcast::Receiver<TranscriptionUpdate> {
        self.update_tx.subscribe()
    }

    pub async fn publish_transcript(&self, record: TranscriptRecord) -> Result<()> {
        self.persistence_tx
            .send(record)
            .await
            .map_err(|err| anyhow::anyhow!("failed to send transcript: {err}"))
    }

    pub fn start_realtime_transcription(
        &self,
        config: RealtimeSessionConfig,
    ) -> (RealtimeSessionHandle, mpsc::Receiver<TranscriptionUpdate>) {
        let (handle, mut rx) = self.orchestrator.start_realtime_session(config.clone());
        let frame_tx = handle.frame_sender();
        let mut pcm_rx = self
            .audio
            .subscribe_lossless_pcm_frames(config.buffer_capacity);
        let audio = self.audio.clone();
        let updates_bus = self.update_tx.clone();
        let (client_tx, client_rx) = mpsc::channel(config.buffer_capacity);

        tokio::spawn(async move {
            while let Some(frame) = pcm_rx.recv().await {
                if frame_tx.send(frame).await.is_err() {
                    break;
                }
            }

            if let Err(err) = audio.flush_pending().await {
                warn!(
                    target: "session_manager",
                    %err,
                    "failed to flush pending pcm frames",
                );
            }

            loop {
                match timeout(Duration::from_millis(100), pcm_rx.recv()).await {
                    Ok(Some(frame)) => {
                        if frame_tx.send(frame).await.is_err() {
                            break;
                        }
                    }
                    Ok(None) | Err(_) => break,
                }
            }
        });

        tokio::spawn(async move {
            while let Some(update) = rx.recv().await {
                let guarantee_delivery = matches!(
                    update.payload,
                    UpdatePayload::Notice(SessionNotice {
                        level: NoticeLevel::Warn | NoticeLevel::Error,
                        ..
                    })
                );

                if let Err(err) = updates_bus.send(update.clone()) {
                    warn!(
                        target: "session_manager",
                        %err,
                        "failed to broadcast session update"
                    );
                }

                match client_tx.try_send(update.clone()) {
                    Ok(()) => {}
                    Err(mpsc::error::TrySendError::Full(update)) => {
                        if guarantee_delivery {
                            if client_tx.send(update).await.is_err() {
                                break;
                            }
                        } else {
                            warn!(
                                target: "session_manager",
                                "dropping realtime session update due to slow consumer"
                            );
                        }
                    }
                    Err(mpsc::error::TrySendError::Closed(_)) => break,
                }
            }
        });

        (handle, client_rx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestrator::{
        EngineConfig, EngineOrchestrator, NoticeLevel, SpeechEngine, TranscriptSource,
        UpdatePayload,
    };
    use anyhow::anyhow;
    use async_trait::async_trait;
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};
    use tokio::time::{timeout, Duration};

    struct ProgrammedSpeechEngine {
        responses: Mutex<VecDeque<anyhow::Result<String>>>,
    }

    impl ProgrammedSpeechEngine {
        fn new(responses: Vec<anyhow::Result<String>>) -> Self {
            Self {
                responses: Mutex::new(responses.into()),
            }
        }
    }

    #[async_trait]
    impl SpeechEngine for ProgrammedSpeechEngine {
        async fn transcribe(&self, _frame: &[f32]) -> anyhow::Result<String> {
            match self
                .responses
                .lock()
                .expect("responses lock poisoned")
                .pop_front()
            {
                Some(result) => result,
                None => Ok(String::new()),
            }
        }
    }

    #[tokio::test]
    async fn routes_audio_frames_and_broadcasts_updates() {
        let local_engine = Arc::new(ProgrammedSpeechEngine::new(vec![Ok("local.".to_string())]));
        let orchestrator = EngineOrchestrator::with_engine(
            EngineConfig {
                prefer_cloud: false,
            },
            local_engine,
        );
        let manager = SessionManager::with_orchestrator(orchestrator);
        manager.run().await.expect("bootstrap should succeed");

        let mut broadcast_rx = manager.subscribe_updates();
        let mut config = RealtimeSessionConfig::default();
        config.enable_polisher = false;
        let (handle, mut client_rx) = manager.start_realtime_transcription(config);

        // Keep the handle alive for the duration of the test.
        let _guard = handle;

        let audio = manager.audio_pipeline();
        audio
            .push_pcm_frame(vec![0.25_f32; 1_600])
            .await
            .expect("push pcm frame");

        let update = timeout(Duration::from_millis(600), client_rx.recv())
            .await
            .expect("client channel timed out")
            .expect("client channel closed");

        let transcript = match &update.payload {
            UpdatePayload::Transcript(payload) => payload,
            _ => panic!("expected transcript payload"),
        };

        assert_eq!(update.frame_index, 1);
        assert_eq!(transcript.source, TranscriptSource::Local);
        assert!(transcript.is_primary);
        assert_eq!(transcript.text, "local.");

        let broadcast_update = timeout(Duration::from_millis(600), broadcast_rx.recv())
            .await
            .expect("broadcast channel timed out")
            .expect("broadcast channel closed");

        assert_eq!(broadcast_update.frame_index, update.frame_index);
        match broadcast_update.payload {
            UpdatePayload::Transcript(_) => {}
            _ => panic!("expected broadcast transcript payload"),
        }
    }

    #[tokio::test]
    async fn delivers_warn_notice_to_slow_clients() {
        let local_engine = Arc::new(ProgrammedSpeechEngine::new(vec![
            Ok("local-fast.".to_string()),
            Err(anyhow!("local failure")),
        ]));
        let cloud_engine = Arc::new(ProgrammedSpeechEngine::new(vec![Ok("cloud.".to_string())]));
        let orchestrator = EngineOrchestrator::with_engines(
            EngineConfig {
                prefer_cloud: false,
            },
            local_engine,
            Some(cloud_engine),
        );
        let manager = SessionManager::with_orchestrator(orchestrator);
        manager.run().await.expect("bootstrap should succeed");

        let mut broadcast_rx = manager.subscribe_updates();
        let mut config = RealtimeSessionConfig::default();
        config.buffer_capacity = 1;
        config.enable_polisher = false;
        let (handle, mut client_rx) = manager.start_realtime_transcription(config);
        let _guard = handle;

        let audio = manager.audio_pipeline();
        audio
            .push_pcm_frame(vec![0.5_f32; 1_600])
            .await
            .expect("push first frame");
        audio
            .push_pcm_frame(vec![0.5_f32; 1_600])
            .await
            .expect("push second frame");

        // Drain the first transcript after both frames are queued so the WARN notice
        // arrives while the client queue is full.
        let first_update = timeout(Duration::from_millis(600), client_rx.recv())
            .await
            .expect("first client update timed out")
            .expect("client channel closed");
        match &first_update.payload {
            UpdatePayload::Transcript(payload) => {
                assert_eq!(payload.text, "local-fast.");
                assert_eq!(payload.source, TranscriptSource::Local);
            }
            _ => panic!("expected first transcript"),
        }

        let warn_update = timeout(Duration::from_millis(800), client_rx.recv())
            .await
            .expect("warn update timed out")
            .expect("warn update missing");
        match &warn_update.payload {
            UpdatePayload::Notice(notice) => {
                assert_eq!(notice.level, NoticeLevel::Error);
                assert!(notice.message.contains("切换云端"));
            }
            _ => panic!("expected warn/error notice"),
        }

        // Broadcast channel should also see the WARN/Error notice.
        let broadcast_notice = timeout(Duration::from_millis(800), async {
            loop {
                match broadcast_rx.recv().await {
                    Ok(update) => match update.payload {
                        UpdatePayload::Notice(notice) => break notice,
                        _ => continue,
                    },
                    Err(err) => panic!("broadcast closed: {err}"),
                }
            }
        })
        .await
        .expect("broadcast timed out");
        assert!(matches!(
            broadcast_notice.level,
            NoticeLevel::Warn | NoticeLevel::Error
        ));
    }
}
