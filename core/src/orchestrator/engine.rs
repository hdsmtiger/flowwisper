use anyhow::Result;
use async_trait::async_trait;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{error, info};

use crate::orchestrator::config::{EngineConfig, RealtimeSessionConfig};
use crate::orchestrator::constants::SILENCE_RMS_THRESHOLD;
use crate::orchestrator::runtime::{self, RealtimeSessionHandle};
use crate::orchestrator::traits::{LightweightSentencePolisher, SentencePolisher, SpeechEngine};
use crate::orchestrator::types::TranscriptionUpdate;

pub struct EngineOrchestrator {
    config: EngineConfig,
    local_engine: Arc<dyn SpeechEngine>,
    cloud_engine: Option<Arc<dyn SpeechEngine>>,
    polisher: Arc<dyn SentencePolisher>,
}

impl EngineOrchestrator {
    pub fn new(config: EngineConfig) -> Result<Self> {
        let local_engine = Self::build_local_engine()?;
        Ok(Self::with_components(
            config,
            local_engine,
            None,
            Arc::new(LightweightSentencePolisher::default()),
        ))
    }

    pub fn with_engine(config: EngineConfig, local_engine: Arc<dyn SpeechEngine>) -> Self {
        Self::with_components(
            config,
            local_engine,
            None,
            Arc::new(LightweightSentencePolisher::default()),
        )
    }

    pub fn with_engines(
        config: EngineConfig,
        local_engine: Arc<dyn SpeechEngine>,
        cloud_engine: Option<Arc<dyn SpeechEngine>>,
    ) -> Self {
        Self::with_components(
            config,
            local_engine,
            cloud_engine,
            Arc::new(LightweightSentencePolisher::default()),
        )
    }

    pub fn with_components(
        config: EngineConfig,
        local_engine: Arc<dyn SpeechEngine>,
        cloud_engine: Option<Arc<dyn SpeechEngine>>,
        polisher: Arc<dyn SentencePolisher>,
    ) -> Self {
        Self {
            config,
            local_engine,
            cloud_engine,
            polisher,
        }
    }

    pub async fn warmup(&self) -> Result<()> {
        info!(
            target: "engine_orchestrator",
            prefer_cloud = self.config.prefer_cloud,
            "warming up engines"
        );
        Ok(())
    }

    pub fn start_realtime_session(
        &self,
        config: RealtimeSessionConfig,
    ) -> (RealtimeSessionHandle, mpsc::Receiver<TranscriptionUpdate>) {
        runtime::spawn_session(
            config,
            Arc::clone(&self.local_engine),
            self.cloud_engine.clone(),
            Arc::clone(&self.polisher),
            self.config.prefer_cloud,
        )
    }

    fn build_local_engine() -> Result<Arc<dyn SpeechEngine>> {
        #[cfg(feature = "local-asr")]
        {
            return match whisper::WhisperLocalEngine::from_env() {
                Ok(engine) => Ok(Arc::new(engine)),
                Err(err) => {
                    if std::env::var("WHISPER_ALLOW_FALLBACK").is_ok() {
                        error!(
                            target: "engine_orchestrator",
                            %err,
                            "failed to initialise whisper local engine, falling back due to WHISPER_ALLOW_FALLBACK"
                        );
                        Ok(Arc::new(FallbackSpeechEngine::default()))
                    } else {
                        error!(
                            target: "engine_orchestrator",
                            %err,
                            "failed to initialise whisper local engine"
                        );
                        Err(err)
                    }
                }
            };
        }

        #[cfg(not(feature = "local-asr"))]
        {
            return Ok(Arc::new(FallbackSpeechEngine::default()));
        }
    }
}

#[derive(Default)]
struct FallbackSpeechEngine {
    counter: AtomicUsize,
}

#[async_trait]
impl SpeechEngine for FallbackSpeechEngine {
    async fn transcribe(&self, frame: &[f32]) -> Result<String> {
        if frame.is_empty() {
            return Ok(String::new());
        }

        let rms = frame_rms(frame);

        if rms <= SILENCE_RMS_THRESHOLD {
            return Ok(String::new());
        }

        let index = self.counter.fetch_add(1, Ordering::SeqCst) + 1;
        Ok(format!("frame#{index}:{rms:.3}"))
    }
}

fn frame_rms(frame: &[f32]) -> f32 {
    if frame.is_empty() {
        return 0.0;
    }

    let energy: f32 = frame.iter().map(|sample| sample * sample).sum();
    (energy / frame.len() as f32).sqrt()
}

#[cfg(feature = "local-asr")]
pub(crate) mod whisper;
