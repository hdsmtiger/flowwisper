//! 会话管理状态机脚手架。

pub mod clipboard;
pub mod lifecycle;
pub mod publisher;

use crate::audio::AudioPipeline;
use crate::orchestrator::{
    EngineConfig, EngineOrchestrator, NoticeLevel, RealtimeSessionConfig, RealtimeSessionHandle,
    SessionNotice, TranscriptionUpdate, UpdatePayload,
};
use crate::persistence::{
    DraftRecord, DraftSaveRequest, NoticeSaveRequest, PersistenceActor, PersistenceCommand,
    PersistenceHandle, TranscriptRecord,
};
use crate::session::clipboard::{ClipboardFallback, ClipboardManager};
use crate::session::lifecycle::{
    SessionLifecyclePayload, SessionLifecyclePhase, SessionLifecycleUpdate,
};
use crate::session::publisher::{
    FallbackStrategy, FocusWindowContext, PublishOutcome, PublishRequest, PublishStrategy,
    Publisher, PublisherFailure, PublisherFailureCode, PublisherStatus, SessionPublisher,
};
use crate::telemetry::events::{
    record_session_draft_failed, record_session_draft_saved, record_session_publish_attempt,
    record_session_publish_degradation, record_session_publish_failure,
    record_session_publish_outcome,
};
use anyhow::Result;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::{broadcast, mpsc, Mutex};
use tokio::time::{timeout, Duration};
use tracing::{info, warn};

const CLIPBOARD_FALLBACK_TIMEOUT_MS: u64 = 200;
const NOTICE_ACTION_COPY: &str = "copy";
const NOTICE_RESULT_SUCCESS: &str = "success";
const NOTICE_RESULT_FAILURE: &str = "failure";

pub struct SessionManager {
    audio: AudioPipeline,
    orchestrator: EngineOrchestrator,
    persistence: PersistenceHandle,
    update_tx: broadcast::Sender<TranscriptionUpdate>,
    lifecycle_tx: broadcast::Sender<SessionLifecycleUpdate>,
    publisher: Arc<dyn SessionPublisher>,
    clipboard: ClipboardManager,
    clipboard_fallback: Arc<Mutex<Option<ClipboardFallback>>>,
}

impl SessionManager {
    pub fn new() -> Result<Self> {
        let audio = AudioPipeline::new();
        let orchestrator = EngineOrchestrator::new(EngineConfig {
            prefer_cloud: false,
        })?;
        Ok(Self::from_parts(
            audio,
            orchestrator,
            Arc::new(Publisher::default()),
            ClipboardManager::with_system(),
        ))
    }

    pub fn with_orchestrator(orchestrator: EngineOrchestrator) -> Self {
        let audio = AudioPipeline::new();
        Self::from_parts(
            audio,
            orchestrator,
            Arc::new(Publisher::default()),
            ClipboardManager::with_system(),
        )
    }

    pub fn with_orchestrator_and_publisher(
        orchestrator: EngineOrchestrator,
        publisher: Arc<dyn SessionPublisher>,
    ) -> Self {
        let audio = AudioPipeline::new();
        Self::from_parts(
            audio,
            orchestrator,
            publisher,
            ClipboardManager::with_system(),
        )
    }

    fn from_parts(
        audio: AudioPipeline,
        orchestrator: EngineOrchestrator,
        publisher: Arc<dyn SessionPublisher>,
        clipboard: ClipboardManager,
    ) -> Self {
        let (persistence_tx, persistence_rx) = mpsc::channel::<PersistenceCommand>(32);
        let persistence = PersistenceHandle::new(persistence_tx);
        let (update_tx, _) = broadcast::channel(64);
        let (lifecycle_tx, _) = broadcast::channel(32);

        tokio::spawn(PersistenceActor::new(persistence_rx).run());

        Self {
            audio,
            orchestrator,
            persistence,
            update_tx,
            lifecycle_tx,
            publisher,
            clipboard,
            clipboard_fallback: Arc::new(Mutex::new(None)),
        }
    }

    #[cfg(test)]
    pub fn with_components(
        orchestrator: EngineOrchestrator,
        publisher: Arc<dyn SessionPublisher>,
        clipboard: ClipboardManager,
    ) -> Self {
        let audio = AudioPipeline::new();
        Self::from_parts(audio, orchestrator, publisher, clipboard)
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

    pub fn subscribe_lifecycle(&self) -> broadcast::Receiver<SessionLifecycleUpdate> {
        self.lifecycle_tx.subscribe()
    }

    async fn persist_transcript(&self, record: TranscriptRecord) -> Result<()> {
        self.persistence
            .save_transcript(record)
            .await
            .map_err(|err| anyhow::anyhow!("failed to persist transcript: {err}"))
    }

    fn emit_lifecycle(&self, update: SessionLifecycleUpdate) {
        if let Err(err) = self.lifecycle_tx.send(update) {
            warn!(
                target: "session_manager",
                %err,
                "failed to broadcast lifecycle update"
            );
        }
    }

    pub async fn publish_transcript(
        &self,
        record: TranscriptRecord,
        request: PublishRequest,
    ) -> Result<PublishOutcome> {
        let session_id = record.session_id.clone();
        self.persist_transcript(record).await?;

        let focus_context = request.focus.clone();
        let fallback_strategy = request.fallback.clone();
        let transcript = request.transcript.clone();

        let fallback = fallback_option(&fallback_strategy);
        self.emit_lifecycle(SessionLifecycleUpdate::publishing(
            &session_id,
            1,
            PublishStrategy::DirectInsert,
            fallback.clone(),
        ));

        record_session_publish_attempt(
            &session_id,
            focus_context.app_identifier.as_deref(),
            focus_context.window_title.as_deref(),
            fallback_strategy.as_str(),
        );

        match self.publisher.publish(request).await {
            Ok(mut outcome) => {
                if outcome.status == PublisherStatus::Failed
                    && matches!(fallback_strategy, FallbackStrategy::ClipboardCopy)
                {
                    outcome = self
                        .attempt_clipboard_fallback(
                            &session_id,
                            &transcript,
                            &fallback_strategy,
                            outcome,
                        )
                        .await;
                }

                let phase = outcome.status.as_phase();
                match phase {
                    SessionLifecyclePhase::Completed => {
                        self.emit_lifecycle(SessionLifecycleUpdate::completed(
                            &session_id,
                            outcome.clone(),
                        ));
                    }
                    SessionLifecyclePhase::Failed => {
                        let (message, code) = outcome
                            .failure
                            .as_ref()
                            .map(|failure| {
                                (
                                    failure.message.clone(),
                                    Some(failure.code.as_str().to_string()),
                                )
                            })
                            .unwrap_or_else(|| ("publisher reported failure".to_string(), None));

                        self.emit_lifecycle(SessionLifecycleUpdate::failed(
                            &session_id,
                            outcome.attempts.max(1),
                            message.clone(),
                            code.clone(),
                            outcome.fallback.clone(),
                        ));

                        record_session_publish_failure(
                            &session_id,
                            message,
                            outcome.attempts.max(1),
                            outcome.fallback.as_ref().map(FallbackStrategy::as_str),
                        );
                    }
                    SessionLifecyclePhase::Publishing => {
                        self.emit_lifecycle(SessionLifecycleUpdate::new(
                            &session_id,
                            SessionLifecyclePhase::Publishing,
                        ));
                    }
                    other => {
                        self.emit_lifecycle(SessionLifecycleUpdate::new(&session_id, other));
                    }
                }

                record_session_publish_outcome(
                    &session_id,
                    outcome.status.as_str(),
                    outcome.strategy.as_str(),
                    outcome.attempts,
                    outcome.fallback.as_ref().map(FallbackStrategy::as_str),
                );

                Ok(outcome)
            }
            Err(err) => {
                self.emit_lifecycle(SessionLifecycleUpdate::failed(
                    &session_id,
                    1,
                    err.to_string(),
                    None,
                    fallback.clone(),
                ));
                record_session_publish_failure(
                    &session_id,
                    err.to_string(),
                    1,
                    fallback.as_ref().map(FallbackStrategy::as_str),
                );
                Err(anyhow::anyhow!(err))
            }
        }
    }

    pub async fn save_transcript_draft(&self, request: DraftSaveRequest) -> Result<DraftRecord> {
        let session_id = request.session_id.clone();
        match self.persistence.save_draft(request).await {
            Ok(record) => {
                record_session_draft_saved(&session_id, &record.draft_id, &record.tags);
                Ok(record)
            }
            Err(err) => {
                record_session_draft_failed(&session_id, err.to_string());
                Err(err)
            }
        }
    }

    async fn attempt_clipboard_fallback(
        &self,
        session_id: &str,
        transcript: &str,
        fallback_strategy: &FallbackStrategy,
        mut outcome: PublishOutcome,
    ) -> PublishOutcome {
        match self
            .clipboard
            .write_with_backup(
                transcript,
                Duration::from_millis(CLIPBOARD_FALLBACK_TIMEOUT_MS),
            )
            .await
        {
            Ok(fallback_handle) => {
                info!(
                    target: "session_manager",
                    session_id,
                    "clipboard fallback executed"
                );

                {
                    let mut guard = self.clipboard_fallback.lock().await;
                    *guard = Some(fallback_handle);
                }

                if let Some(failure) = &outcome.failure {
                    record_session_publish_failure(
                        session_id,
                        failure.message.clone(),
                        outcome.attempts.max(1),
                        Some(fallback_strategy.as_str()),
                    );
                }

                let message =
                    "自动降级：已将润色稿复制到剪贴板，请切换至目标窗口粘贴（原内容已备份）。"
                        .to_string();
                self.emit_notice(NoticeLevel::Warn, message.clone());
                self.persist_notice_entry(
                    session_id,
                    NOTICE_ACTION_COPY,
                    NOTICE_RESULT_SUCCESS,
                    NoticeLevel::Warn,
                    message,
                    None,
                )
                .await;
                record_session_publish_degradation(
                    session_id,
                    fallback_strategy.as_str(),
                    NOTICE_RESULT_SUCCESS,
                );

                outcome.status = PublisherStatus::Deferred;
                outcome.strategy = PublishStrategy::ClipboardFallback;
                outcome.fallback = Some(fallback_strategy.clone());
                outcome
            }
            Err(err) => {
                warn!(
                    target: "session_manager",
                    %err,
                    "clipboard fallback failed"
                );

                let fallback_error = format!("剪贴板复制失败: {err}");
                match outcome.failure.as_mut() {
                    Some(failure) => {
                        failure.message = format!("{}; {fallback_error}", failure.message);
                    }
                    None => {
                        outcome.failure = Some(PublisherFailure::new(
                            PublisherFailureCode::Unknown,
                            fallback_error.clone(),
                        ));
                    }
                }

                let message =
                    format!("自动降级失败：无法复制润色稿到剪贴板，请手动复制。错误: {err}");
                self.emit_notice(NoticeLevel::Error, message.clone());
                self.persist_notice_entry(
                    session_id,
                    NOTICE_ACTION_COPY,
                    NOTICE_RESULT_FAILURE,
                    NoticeLevel::Error,
                    message,
                    None,
                )
                .await;
                record_session_publish_degradation(
                    session_id,
                    fallback_strategy.as_str(),
                    NOTICE_RESULT_FAILURE,
                );

                outcome
            }
        }
    }

    fn emit_notice<S: Into<String>>(&self, level: NoticeLevel, message: S) {
        let update = TranscriptionUpdate {
            payload: UpdatePayload::Notice(SessionNotice {
                level,
                message: message.into(),
            }),
            latency: Duration::from_millis(0),
            frame_index: 0,
            is_first: false,
        };

        if let Err(err) = self.update_tx.send(update) {
            warn!(
                target: "session_manager",
                %err,
                "failed to broadcast clipboard notice"
            );
        }
    }

    async fn persist_notice_entry(
        &self,
        session_id: &str,
        action: &str,
        result: &str,
        level: NoticeLevel,
        message: String,
        undo_token: Option<String>,
    ) {
        let request = NoticeSaveRequest {
            notice_id: make_notice_id(session_id),
            session_id: session_id.to_string(),
            action: action.to_string(),
            result: result.to_string(),
            level: notice_level_value(level).to_string(),
            message,
            undo_token,
        };

        if let Err(err) = self.persistence.save_notice(request).await {
            warn!(
                target: "session_manager",
                %err,
                "failed to persist publish notice"
            );
        }
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

    #[cfg(test)]
    pub fn persistence_handle(&self) -> PersistenceHandle {
        self.persistence.clone()
    }
}

fn fallback_option(strategy: &FallbackStrategy) -> Option<FallbackStrategy> {
    match strategy {
        FallbackStrategy::None => None,
        other => Some(other.clone()),
    }
}

fn make_notice_id(session_id: &str) -> String {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    format!("{session_id}-notice-{timestamp}")
}

fn notice_level_value(level: NoticeLevel) -> &'static str {
    match level {
        NoticeLevel::Info => "info",
        NoticeLevel::Warn => "warn",
        NoticeLevel::Error => "error",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orchestrator::{
        EngineConfig, EngineOrchestrator, NoticeLevel, SpeechEngine, TranscriptSource,
        UpdatePayload,
    };
    use crate::session::clipboard::{ClipboardAccess, ClipboardError, ClipboardManager};
    use crate::session::publisher::PublisherError;
    use anyhow::anyhow;
    use async_trait::async_trait;
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};
    use tokio::sync::Mutex as AsyncMutex;
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

    #[derive(Clone)]
    struct StubPublisher {
        outcome: PublishOutcome,
    }

    impl StubPublisher {
        fn new(outcome: PublishOutcome) -> Self {
            Self { outcome }
        }
    }

    #[async_trait]
    impl SessionPublisher for StubPublisher {
        async fn publish(
            &self,
            _request: PublishRequest,
        ) -> Result<PublishOutcome, PublisherError> {
            Ok(self.outcome.clone())
        }
    }

    #[derive(Clone, Default)]
    struct RecordingClipboard {
        state: Arc<AsyncMutex<Option<String>>>,
        write_error: Arc<AsyncMutex<Option<ClipboardError>>>,
    }

    impl RecordingClipboard {
        async fn contents(&self) -> Option<String> {
            self.state.lock().await.clone()
        }

        async fn set_write_error(&self, error: ClipboardError) {
            *self.write_error.lock().await = Some(error);
        }
    }

    #[async_trait]
    impl ClipboardAccess for RecordingClipboard {
        async fn read_text(&self, _timeout: Duration) -> Result<Option<String>, ClipboardError> {
            Ok(self.state.lock().await.clone())
        }

        async fn write_text(
            &self,
            contents: &str,
            _timeout: Duration,
        ) -> Result<(), ClipboardError> {
            if let Some(err) = self.write_error.lock().await.clone() {
                return Err(err);
            }

            *self.state.lock().await = Some(contents.to_string());
            Ok(())
        }

        async fn clear(&self, _timeout: Duration) -> Result<(), ClipboardError> {
            *self.state.lock().await = None;
            Ok(())
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

    #[tokio::test]
    async fn publishes_transcript_and_emits_lifecycle_updates() {
        let local_engine = Arc::new(ProgrammedSpeechEngine::new(vec![Ok("local.".into())]));
        let orchestrator = EngineOrchestrator::with_engine(
            EngineConfig {
                prefer_cloud: false,
            },
            local_engine,
        );
        let manager = SessionManager::with_orchestrator(orchestrator);

        let mut lifecycle_rx = manager.subscribe_lifecycle();
        let record = TranscriptRecord {
            session_id: "session-1".into(),
            raw_text: "raw".into(),
            polished_text: "polished".into(),
        };
        let request = PublishRequest {
            transcript: "polished".into(),
            focus: FocusWindowContext::from_app_identifier("com.example.app"),
            fallback: FallbackStrategy::ClipboardCopy,
        };

        let outcome = manager
            .publish_transcript(record, request)
            .await
            .expect("publish should succeed");

        assert_eq!(outcome.status, PublisherStatus::Completed);
        assert_eq!(outcome.strategy, PublishStrategy::DirectInsert);

        let publishing_update = lifecycle_rx
            .recv()
            .await
            .expect("publishing update missing");
        assert_eq!(publishing_update.phase, SessionLifecyclePhase::Publishing);

        let completion_update = lifecycle_rx
            .recv()
            .await
            .expect("completion update missing");
        assert_eq!(completion_update.phase, SessionLifecyclePhase::Completed);
        match completion_update.payload {
            SessionLifecyclePayload::Completed(payload) => {
                assert_eq!(payload.outcome.status, PublisherStatus::Completed);
            }
            other => panic!("unexpected payload: {other:?}"),
        }
    }

    #[tokio::test]
    async fn surfaces_publisher_errors_and_emits_failure_update() {
        let local_engine = Arc::new(ProgrammedSpeechEngine::new(vec![Ok("local.".into())]));
        let orchestrator = EngineOrchestrator::with_engine(
            EngineConfig {
                prefer_cloud: false,
            },
            local_engine,
        );
        let manager = SessionManager::with_orchestrator(orchestrator);

        let mut lifecycle_rx = manager.subscribe_lifecycle();
        let record = TranscriptRecord {
            session_id: "session-2".into(),
            raw_text: "raw".into(),
            polished_text: "".into(),
        };
        let request = PublishRequest {
            transcript: "   ".into(),
            focus: FocusWindowContext::default(),
            fallback: FallbackStrategy::NotifyOnly,
        };

        let result = manager.publish_transcript(record, request).await;
        assert!(result.is_err());

        let publishing_update = lifecycle_rx
            .recv()
            .await
            .expect("publishing update missing");
        assert_eq!(publishing_update.phase, SessionLifecyclePhase::Publishing);

        let failure_update = lifecycle_rx.recv().await.expect("failure update missing");
        assert_eq!(failure_update.phase, SessionLifecyclePhase::Failed);
        match failure_update.payload {
            SessionLifecyclePayload::Failed(payload) => {
                assert_eq!(payload.attempts, 1);
                assert_eq!(payload.fallback, Some(FallbackStrategy::NotifyOnly));
            }
            other => panic!("unexpected payload: {other:?}"),
        }
    }

    #[tokio::test]
    async fn clipboard_fallback_copies_and_notifies() {
        let orchestrator = EngineOrchestrator::with_engine(
            EngineConfig {
                prefer_cloud: false,
            },
            Arc::new(ProgrammedSpeechEngine::new(Vec::new())),
        );

        let failure = PublisherFailure::new(PublisherFailureCode::Timeout, "operation timed out");
        let outcome = PublishOutcome {
            status: PublisherStatus::Failed,
            strategy: PublishStrategy::DirectInsert,
            attempts: 2,
            fallback: None,
            failure: Some(failure.clone()),
        };

        let publisher = Arc::new(StubPublisher::new(outcome));
        let clipboard_access = RecordingClipboard::default();
        let clipboard = ClipboardManager::new(Arc::new(clipboard_access.clone()));
        let manager = SessionManager::with_components(orchestrator, publisher, clipboard);

        let mut updates_rx = manager.subscribe_updates();
        let record = TranscriptRecord {
            session_id: "session-fallback".into(),
            raw_text: "raw".into(),
            polished_text: "polished".into(),
        };
        let request = PublishRequest {
            transcript: "polished".into(),
            focus: FocusWindowContext::default(),
            fallback: FallbackStrategy::ClipboardCopy,
        };

        let outcome = manager
            .publish_transcript(record, request)
            .await
            .expect("publish should succeed");

        assert_eq!(outcome.status, PublisherStatus::Deferred);
        assert_eq!(outcome.strategy, PublishStrategy::ClipboardFallback);
        assert_eq!(outcome.fallback, Some(FallbackStrategy::ClipboardCopy));
        match outcome.failure {
            Some(ref failure) => assert_eq!(failure.code, PublisherFailureCode::Timeout),
            None => panic!("expected failure context"),
        }

        let clipboard_contents = clipboard_access.contents().await;
        assert_eq!(clipboard_contents.as_deref(), Some("polished"));

        let notice = updates_rx.recv().await.expect("notice missing");
        match notice.payload {
            UpdatePayload::Notice(SessionNotice { level, message }) => {
                assert_eq!(level, NoticeLevel::Warn);
                assert!(message.contains("已将润色稿复制到剪贴板"));
            }
            _ => panic!("expected clipboard fallback notice"),
        }

        let notices = manager
            .persistence_handle()
            .list_notices(10)
            .await
            .expect("persisted notices available");
        assert!(notices.iter().any(
            |entry| entry.action == NOTICE_ACTION_COPY && entry.result == NOTICE_RESULT_SUCCESS
        ));
    }

    #[tokio::test]
    async fn clipboard_fallback_failure_updates_outcome() {
        let orchestrator = EngineOrchestrator::with_engine(
            EngineConfig {
                prefer_cloud: false,
            },
            Arc::new(ProgrammedSpeechEngine::new(Vec::new())),
        );

        let failure = PublisherFailure::new(PublisherFailureCode::Timeout, "operation timed out");
        let outcome = PublishOutcome {
            status: PublisherStatus::Failed,
            strategy: PublishStrategy::DirectInsert,
            attempts: 1,
            fallback: None,
            failure: Some(failure),
        };

        let publisher = Arc::new(StubPublisher::new(outcome));
        let clipboard_access = RecordingClipboard::default();
        clipboard_access
            .set_write_error(ClipboardError::write("permission denied"))
            .await;
        let clipboard = ClipboardManager::new(Arc::new(clipboard_access.clone()));
        let manager = SessionManager::with_components(orchestrator, publisher, clipboard);

        let mut updates_rx = manager.subscribe_updates();
        let record = TranscriptRecord {
            session_id: "session-fallback-fail".into(),
            raw_text: "raw".into(),
            polished_text: "polished".into(),
        };
        let request = PublishRequest {
            transcript: "polished".into(),
            focus: FocusWindowContext::default(),
            fallback: FallbackStrategy::ClipboardCopy,
        };

        let outcome = manager
            .publish_transcript(record, request)
            .await
            .expect("publish should surface fallback failure");

        assert_eq!(outcome.status, PublisherStatus::Failed);
        let failure = outcome.failure.expect("failure details missing");
        assert!(failure.message.contains("operation timed out"));
        assert!(failure.message.contains("剪贴板复制失败"));

        let notice = updates_rx.recv().await.expect("failure notice missing");
        match notice.payload {
            UpdatePayload::Notice(SessionNotice { level, message }) => {
                assert_eq!(level, NoticeLevel::Error);
                assert!(message.contains("自动降级失败"));
                assert!(message.contains("手动复制"));
            }
            _ => panic!("expected clipboard failure notice"),
        }

        let notices = manager
            .persistence_handle()
            .list_notices(10)
            .await
            .expect("persisted notices available");
        assert!(notices.iter().any(
            |entry| entry.action == NOTICE_ACTION_COPY && entry.result == NOTICE_RESULT_FAILURE
        ));
    }

    #[tokio::test]
    async fn saves_transcript_draft_and_records_history() {
        let orchestrator = EngineOrchestrator::with_engine(
            EngineConfig {
                prefer_cloud: false,
            },
            Arc::new(ProgrammedSpeechEngine::new(Vec::new())),
        );
        let manager = SessionManager::with_orchestrator(orchestrator);

        let request = DraftSaveRequest {
            draft_id: "draft-001".into(),
            session_id: "session-save".into(),
            content: "Draft body".into(),
            title: None,
            tags: None,
        };

        let record = manager
            .save_transcript_draft(request)
            .await
            .expect("draft save should succeed");
        assert_eq!(record.title, "Polished transcript");
        assert_eq!(record.tags, vec!["transcript".to_string()]);

        let drafts = manager
            .persistence_handle()
            .list_drafts(5)
            .await
            .expect("draft history available");
        assert!(drafts.iter().any(|draft| draft.draft_id == "draft-001"));
    }
}
