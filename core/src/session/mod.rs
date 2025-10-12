//! 会话管理状态机脚手架。

pub mod clipboard;
pub mod history;
pub mod lifecycle;
pub mod publisher;

use crate::audio::AudioPipeline;
use crate::orchestrator::{
    EngineConfig, EngineOrchestrator, NoticeLevel, RealtimeSessionConfig, RealtimeSessionHandle,
    SessionNotice, TranscriptionUpdate, UpdatePayload,
};
use crate::persistence::sqlite::{EnvKeyResolver, SqliteConfig, SqlitePath, SqlitePersistence};
use crate::persistence::{
    DraftRecord, DraftSaveRequest, NoticeSaveRequest, PersistenceActor, PersistenceCommand,
    PersistenceHandle,
};
use crate::session::clipboard::{ClipboardFallback, ClipboardManager};
use crate::session::history::{
    AccuracyUpdate, HistoryEntry, HistoryPage, HistoryPostAction, HistoryQuery, SessionSnapshot,
};
use crate::session::lifecycle::{SessionLifecyclePhase, SessionLifecycleUpdate};
use crate::session::publisher::{
    FallbackStrategy, PublishOutcome, PublishRequest, PublishStrategy, Publisher,
    PublisherFailure, PublisherFailureCode, PublisherStatus, SessionPublisher,
};
use crate::telemetry::events::{
    record_session_draft_failed, record_session_draft_saved, record_session_noise_warning,
    record_session_publish_attempt, record_session_publish_degradation,
    record_session_publish_failure, record_session_publish_outcome,
    record_session_silence_autostop, record_session_silence_countdown, EVENT_NOISE_WARNING,
    EVENT_SILENCE_AUTOSTOP, EVENT_SILENCE_COUNTDOWN,
};
use anyhow::{anyhow, Context, Result};
use dirs::data_dir;
use serde_json::json;
use std::env;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration as StdDuration, SystemTime, UNIX_EPOCH};
use tokio::sync::{
    broadcast::{self, error::RecvError},
    mpsc, Mutex,
};
use tokio::time::{interval, timeout, Duration};
use tracing::{error, info, warn};

const CLIPBOARD_FALLBACK_TIMEOUT_MS: u64 = 200;
const NOTICE_ACTION_COPY: &str = "copy";
const NOTICE_RESULT_SUCCESS: &str = "success";
const NOTICE_RESULT_FAILURE: &str = "failure";
const HISTORY_CLEANUP_INTERVAL_SECS: u64 = 30 * 60;

#[derive(Debug, Clone)]
pub enum SessionEvent {
    NoiseWarning(SessionNoiseWarning),
    SilenceCountdown(SessionSilenceCountdown),
    AutoStop(SessionAutoStop),
}

#[derive(Debug, Clone)]
pub struct SessionNoiseWarning {
    pub baseline_db: f32,
    pub threshold_db: f32,
    pub level_db: f32,
    pub persistence_ms: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SilenceCountdownState {
    Started,
    Tick,
    Canceled,
    Completed,
}

#[derive(Debug, Clone)]
pub struct SessionSilenceCountdown {
    pub total_ms: u32,
    pub remaining_ms: u32,
    pub state: SilenceCountdownState,
    pub cancel_reason: Option<SilenceCancellationReason>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoStopReason {
    SilenceTimeout,
}

#[derive(Debug, Clone)]
pub struct SessionAutoStop {
    pub reason: AutoStopReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SilenceCancellationReason {
    SpeechDetected,
    ManualStop,
}

#[derive(Debug, Clone, Copy)]
struct SilenceCountdownSnapshot {
    total_ms: u32,
    remaining_ms: u32,
}

fn resolve_persistence_config() -> Result<SqliteConfig> {
    let base_dir = match env::var("FLOWWISPER_DATA_DIR").map(PathBuf::from) {
        Ok(path) => path,
        Err(_) => data_dir()
            .map(|dir| dir.join("Flowwisper"))
            .ok_or_else(|| anyhow!("failed to resolve persistence data directory"))?,
    };

    fs::create_dir_all(&base_dir).context("failed to create data directory")?;
    let db_path = base_dir.join("history.db");

    Ok(SqliteConfig {
        path: SqlitePath::File(db_path),
        pool_size: 8,
        busy_timeout: StdDuration::from_millis(250),
        key_resolver: Arc::new(EnvKeyResolver::default()),
    })
}

fn spawn_persistence_runtime(config: SqliteConfig) -> Result<PersistenceHandle> {
    let sqlite = Arc::new(SqlitePersistence::bootstrap(config)?);
    let (tx, rx) = mpsc::channel::<PersistenceCommand>(64);
    let handle = PersistenceHandle::new(tx.clone(), sqlite.clone());

    tokio::spawn(async move {
        if let Err(err) = PersistenceActor::new(sqlite, rx).run().await {
            error!(target: "persistence", %err, "persistence actor exited");
        }
    });

    Ok(handle)
}

pub struct SessionManager {
    audio: AudioPipeline,
    orchestrator: EngineOrchestrator,
    persistence: PersistenceHandle,
    update_tx: broadcast::Sender<TranscriptionUpdate>,
    lifecycle_tx: broadcast::Sender<SessionLifecycleUpdate>,
    event_tx: broadcast::Sender<SessionEvent>,
    publisher: Arc<dyn SessionPublisher>,
    clipboard: ClipboardManager,
    clipboard_fallback: Arc<Mutex<Option<ClipboardFallback>>>,
    history_cleanup_started: AtomicBool,
    silence_countdown_active: Arc<AtomicBool>,
    auto_stop_triggered: Arc<AtomicBool>,
    silence_countdown_snapshot: Arc<Mutex<Option<SilenceCountdownSnapshot>>>,
    active_session_id: Arc<Mutex<Option<String>>>,
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
        let config = resolve_persistence_config().expect("persistence config should resolve");
        let persistence =
            spawn_persistence_runtime(config).expect("persistence runtime should spawn");
        let (update_tx, _) = broadcast::channel(64);
        let (lifecycle_tx, _) = broadcast::channel(32);
        let (event_tx, _) = broadcast::channel(32);
        let silence_countdown_active = Arc::new(AtomicBool::new(false));
        let auto_stop_triggered = Arc::new(AtomicBool::new(false));
        let silence_countdown_snapshot = Arc::new(Mutex::new(None));
        let active_session_id = Arc::new(Mutex::new(None));

        let manager = Self {
            audio,
            orchestrator,
            persistence,
            update_tx,
            lifecycle_tx,
            event_tx,
            publisher,
            clipboard,
            clipboard_fallback: Arc::new(Mutex::new(None)),
            history_cleanup_started: AtomicBool::new(false),
            silence_countdown_active,
            auto_stop_triggered,
            silence_countdown_snapshot,
            active_session_id,
        };

        manager.spawn_noise_listener();

        manager
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
        self.schedule_history_cleanup();
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

    pub fn subscribe_events(&self) -> broadcast::Receiver<SessionEvent> {
        self.event_tx.subscribe()
    }

    pub async fn set_active_session_id<S: Into<String>>(&self, session_id: S) {
        let mut guard = self.active_session_id.lock().await;
        *guard = Some(session_id.into());
    }

    pub async fn clear_active_session_id(&self) {
        let mut guard = self.active_session_id.lock().await;
        *guard = None;
    }

    fn spawn_noise_listener(&self) {
        let mut noise_rx = self.audio.subscribe_noise_events();
        let event_tx = self.event_tx.clone();
        let audio = self.audio.clone();
        let persistence = self.persistence.clone();
        let countdown_active = Arc::clone(&self.silence_countdown_active);
        let auto_stop_triggered = Arc::clone(&self.auto_stop_triggered);
        let snapshot = Arc::clone(&self.silence_countdown_snapshot);
        let active_session_id = Arc::clone(&self.active_session_id);

        tokio::spawn(async move {
            loop {
                match noise_rx.recv().await {
                    Ok(crate::audio::NoiseEvent::NoiseWarning(payload)) => {
                        let event = SessionEvent::NoiseWarning(SessionNoiseWarning {
                            baseline_db: payload.baseline_db,
                            threshold_db: payload.threshold_db,
                            level_db: payload.window_db,
                            persistence_ms: payload.persistence_ms,
                        });

                        let timestamp = SystemTime::now();
                        let session_id = {
                            active_session_id
                                .lock()
                                .await
                                .clone()
                                .unwrap_or_else(|| "unassigned".to_string())
                        };

                        record_session_noise_warning(
                            &session_id,
                            payload.baseline_db,
                            payload.threshold_db,
                            payload.window_db,
                            payload.persistence_ms,
                            false,
                            timestamp,
                        );

                        if let Err(err) = event_tx.send(event) {
                            warn!(
                                target: "session_manager",
                                %err,
                                "failed to broadcast noise warning event",
                            );
                        }

                        let occurred_at_ms = system_time_to_ms(timestamp);
                        let queue_payload = json!({
                            "sessionId": session_id,
                            "occurredAtMs": occurred_at_ms,
                            "baselineDb": payload.baseline_db,
                            "thresholdDb": payload.threshold_db,
                            "levelDb": payload.window_db,
                            "persistenceMs": payload.persistence_ms,
                            "strongNoiseMode": false,
                        });

                        if let Err(err) = persistence
                            .enqueue_telemetry(
                                session_id,
                                EVENT_NOISE_WARNING.to_string(),
                                queue_payload,
                            )
                            .await
                        {
                            warn!(
                                target: "session_manager",
                                %err,
                                "failed to queue noise warning telemetry",
                            );
                        }
                    }
                    Ok(crate::audio::NoiseEvent::SilenceCountdown(payload)) => {
                        let state = match payload.status {
                            crate::audio::SilenceCountdownStatus::Started => {
                                SilenceCountdownState::Started
                            }
                            crate::audio::SilenceCountdownStatus::Tick => {
                                SilenceCountdownState::Tick
                            }
                            crate::audio::SilenceCountdownStatus::Canceled => {
                                SilenceCountdownState::Canceled
                            }
                            crate::audio::SilenceCountdownStatus::Completed => {
                                SilenceCountdownState::Completed
                            }
                        };

                        let mut snapshot_guard = snapshot.lock().await;
                        match state {
                            SilenceCountdownState::Canceled => {
                                *snapshot_guard = None;
                            }
                            _ => {
                                *snapshot_guard = Some(SilenceCountdownSnapshot {
                                    total_ms: payload.total_ms,
                                    remaining_ms: payload.remaining_ms,
                                });
                            }
                        }
                        drop(snapshot_guard);

                        let cancel_reason = if matches!(state, SilenceCountdownState::Canceled) {
                            Some(SilenceCancellationReason::SpeechDetected)
                        } else {
                            None
                        };

                        let countdown_event =
                            SessionEvent::SilenceCountdown(SessionSilenceCountdown {
                                total_ms: payload.total_ms,
                                remaining_ms: payload.remaining_ms,
                                state,
                                cancel_reason,
                            });

                        if let Err(err) = event_tx.send(countdown_event) {
                            warn!(
                                target: "session_manager",
                                %err,
                                "failed to broadcast silence countdown event",
                            );
                        }

                        if !matches!(state, SilenceCountdownState::Tick) {
                            let timestamp = SystemTime::now();
                            let session_id = {
                                active_session_id
                                    .lock()
                                    .await
                                    .clone()
                                    .unwrap_or_else(|| "unassigned".to_string())
                            };
                            let cancel_reason_value = cancel_reason.map(|reason| match reason {
                                SilenceCancellationReason::SpeechDetected => "speechDetected",
                                SilenceCancellationReason::ManualStop => "manualStop",
                            });

                            record_session_silence_countdown(
                                &session_id,
                                countdown_state_label(state),
                                payload.total_ms,
                                payload.remaining_ms,
                                cancel_reason_value,
                                timestamp,
                            );

                            let timestamp_ms = system_time_to_ms(timestamp);
                            let queue_payload = json!({
                                "sessionId": session_id,
                                "timestampMs": timestamp_ms,
                                "state": countdown_state_label(state),
                                "totalMs": payload.total_ms,
                                "remainingMs": payload.remaining_ms,
                                "cancelReason": cancel_reason_value,
                            });

                            if let Err(err) = persistence
                                .enqueue_telemetry(
                                    queue_payload["sessionId"]
                                        .as_str()
                                        .unwrap_or("unassigned")
                                        .to_string(),
                                    EVENT_SILENCE_COUNTDOWN.to_string(),
                                    queue_payload,
                                )
                                .await
                            {
                                warn!(
                                    target: "session_manager",
                                    %err,
                                    "failed to queue silence countdown telemetry",
                                );
                            }
                        }

                        match state {
                            SilenceCountdownState::Started => {
                                countdown_active.store(true, Ordering::SeqCst);
                                auto_stop_triggered.store(false, Ordering::SeqCst);
                            }
                            SilenceCountdownState::Tick => {
                                countdown_active.store(true, Ordering::SeqCst);
                            }
                            SilenceCountdownState::Canceled => {
                                countdown_active.store(false, Ordering::SeqCst);
                                auto_stop_triggered.store(false, Ordering::SeqCst);
                            }
                            SilenceCountdownState::Completed => {
                                countdown_active.store(false, Ordering::SeqCst);
                                let already_triggered =
                                    auto_stop_triggered.swap(true, Ordering::SeqCst);
                                if !already_triggered {
                                    {
                                        let mut guard = snapshot.lock().await;
                                        *guard = None;
                                    }

                                    let auto_stop_event = SessionEvent::AutoStop(SessionAutoStop {
                                        reason: AutoStopReason::SilenceTimeout,
                                    });

                                    if let Err(err) = event_tx.send(auto_stop_event) {
                                        warn!(
                                            target: "session_manager",
                                            %err,
                                            "failed to broadcast auto-stop event",
                                        );
                                    }

                                    audio.reset_session();
                                    info!(
                                        target: "session_manager",
                                        "silence countdown completed; auto-stop triggered",
                                    );

                                    let timestamp = SystemTime::now();
                                    let session_id = {
                                        active_session_id
                                            .lock()
                                            .await
                                            .clone()
                                            .unwrap_or_else(|| "unassigned".to_string())
                                    };

                                    record_session_silence_autostop(
                                        &session_id,
                                        payload.total_ms,
                                        timestamp,
                                    );

                                    let timestamp_ms = system_time_to_ms(timestamp);
                                    let queue_payload = json!({
                                        "sessionId": session_id,
                                        "timestampMs": timestamp_ms,
                                        "reason": "silenceTimeout",
                                        "countdownMs": payload.total_ms,
                                    });

                                    if let Err(err) = persistence
                                        .enqueue_telemetry(
                                            queue_payload["sessionId"]
                                                .as_str()
                                                .unwrap_or("unassigned")
                                                .to_string(),
                                            EVENT_SILENCE_AUTOSTOP.to_string(),
                                            queue_payload,
                                        )
                                        .await
                                    {
                                        warn!(
                                            target: "session_manager",
                                            %err,
                                            "failed to queue silence autostop telemetry",
                                        );
                                    }
                                }
                            }
                        }
                    }
                    Ok(crate::audio::NoiseEvent::BaselineEstablished { .. }) => {
                        countdown_active.store(false, Ordering::SeqCst);
                        auto_stop_triggered.store(false, Ordering::SeqCst);
                        let mut guard = snapshot.lock().await;
                        *guard = None;
                    }
                    Err(RecvError::Lagged(skipped)) => {
                        warn!(
                            target: "session_manager",
                            skipped,
                            "noise event listener lagged",
                        );
                    }
                    Err(RecvError::Closed) => break,
                }
            }
        });
    }

    pub async fn cancel_silence_countdown_due_to_manual_stop(&self) {
        let was_active = self.silence_countdown_active.swap(false, Ordering::SeqCst);
        self.auto_stop_triggered.store(false, Ordering::SeqCst);

        if !was_active {
            return;
        }

        let snapshot = {
            let mut guard = self.silence_countdown_snapshot.lock().await;
            guard.take().unwrap_or(SilenceCountdownSnapshot {
                total_ms: 5_000,
                remaining_ms: 5_000,
            })
        };

        let event = SessionEvent::SilenceCountdown(SessionSilenceCountdown {
            total_ms: snapshot.total_ms,
            remaining_ms: snapshot.remaining_ms,
            state: SilenceCountdownState::Canceled,
            cancel_reason: Some(SilenceCancellationReason::ManualStop),
        });

        if let Err(err) = self.event_tx.send(event) {
            warn!(
                target: "session_manager",
                %err,
                "failed to broadcast manual silence cancellation",
            );
        }

        let timestamp = SystemTime::now();
        let session_id = {
            self.active_session_id
                .lock()
                .await
                .clone()
                .unwrap_or_else(|| "unassigned".to_string())
        };

        record_session_silence_countdown(
            &session_id,
            countdown_state_label(SilenceCountdownState::Canceled),
            snapshot.total_ms,
            snapshot.remaining_ms,
            Some("manualStop"),
            timestamp,
        );

        let queue_payload = json!({
            "sessionId": session_id,
            "timestampMs": system_time_to_ms(timestamp),
            "state": "canceled",
            "totalMs": snapshot.total_ms,
            "remainingMs": snapshot.remaining_ms,
            "cancelReason": "manualStop",
        });

        if let Err(err) = self
            .persistence
            .enqueue_telemetry(
                queue_payload["sessionId"]
                    .as_str()
                    .unwrap_or("unassigned")
                    .to_string(),
                EVENT_SILENCE_COUNTDOWN.to_string(),
                queue_payload,
            )
            .await
        {
            warn!(
                target: "session_manager",
                %err,
                "failed to queue manual silence cancel telemetry",
            );
        }
    }

    async fn persist_transcript(&self, snapshot: SessionSnapshot) -> Result<()> {
        self.persistence
            .persist_session(snapshot)
            .await
            .map_err(|err| anyhow!("failed to persist transcript: {err}"))
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
        snapshot: SessionSnapshot,
        request: PublishRequest,
    ) -> Result<PublishOutcome> {
        let session_id = snapshot.session_id.clone();

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

                if matches!(
                    outcome.status,
                    PublisherStatus::Completed | PublisherStatus::Deferred
                ) {
                    if let Err(err) = self.persist_transcript(snapshot.clone()).await {
                        self.handle_persistence_failure(&snapshot, err).await;
                    }
                }

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

    pub async fn search_history(&self, query: HistoryQuery) -> Result<HistoryPage> {
        self.persistence
            .search_history(query)
            .await
            .map_err(|err| anyhow!("history search failed: {err}"))
    }

    pub async fn load_history_entry(&self, session_id: &str) -> Result<Option<HistoryEntry>> {
        self.persistence
            .load_session(session_id.to_string())
            .await
            .map_err(|err| anyhow!("history load failed: {err}"))
    }

    pub async fn update_history_accuracy(&self, update: AccuracyUpdate) -> Result<()> {
        self.persistence
            .update_accuracy(update)
            .await
            .map_err(|err| anyhow!("failed to update history accuracy: {err}"))
    }

    pub async fn record_history_action(
        &self,
        session_id: String,
        action: HistoryPostAction,
    ) -> Result<Vec<HistoryPostAction>> {
        self.persistence
            .append_post_action(session_id, action)
            .await
            .map_err(|err| anyhow!("failed to append history action: {err}"))
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

    fn schedule_history_cleanup(&self) {
        if self.history_cleanup_started.swap(true, Ordering::SeqCst) {
            return;
        }
        let persistence = self.persistence.clone();
        tokio::spawn(async move {
            let mut ticker = interval(Duration::from_secs(HISTORY_CLEANUP_INTERVAL_SECS));
            loop {
                ticker.tick().await;
                let now_ms = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|duration| duration.as_millis() as i64)
                    .unwrap_or(0);

                if let Err(err) = persistence.cleanup_expired(now_ms).await {
                    warn!(
                        target: "session_manager",
                        %err,
                        "scheduled history cleanup failed"
                    );
                }
            }
        });
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

    async fn handle_persistence_failure(&self, snapshot: &SessionSnapshot, error: anyhow::Error) {
        warn!(
            target: "session_manager",
            session_id = %snapshot.session_id,
            %error,
            "failed to persist session history"
        );

        let mut notice_message = format!("历史记录保存失败：{}。", error);

        let clipboard_result = self
            .clipboard
            .write_with_backup(
                &snapshot.polished_transcript,
                Duration::from_millis(CLIPBOARD_FALLBACK_TIMEOUT_MS),
            )
            .await;

        match clipboard_result {
            Ok(fallback_handle) => {
                {
                    let mut guard = self.clipboard_fallback.lock().await;
                    *guard = Some(fallback_handle);
                }
                notice_message.push_str("已将润色稿复制到剪贴板作为备份。");
            }
            Err(copy_err) => {
                notice_message.push_str("且无法复制到剪贴板，请手动保存文本。");
                warn!(
                    target: "session_manager",
                    session_id = %snapshot.session_id,
                    %copy_err,
                    "clipboard backup for persistence failure failed"
                );
            }
        }

        self.emit_notice(NoticeLevel::Error, notice_message.clone());
        self.persist_notice_entry(
            &snapshot.session_id,
            NOTICE_ACTION_COPY,
            NOTICE_RESULT_FAILURE,
            NoticeLevel::Error,
            notice_message,
            None,
        )
        .await;

        let payload = json!({
            "session_id": snapshot.session_id,
            "error": error.to_string(),
        });

        let _ = self
            .persistence
            .enqueue_telemetry(
                snapshot.session_id.clone(),
                "history_persist_failure".into(),
                payload,
            )
            .await;
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

fn countdown_state_label(state: SilenceCountdownState) -> &'static str {
    match state {
        SilenceCountdownState::Started => "started",
        SilenceCountdownState::Tick => "tick",
        SilenceCountdownState::Canceled => "canceled",
        SilenceCountdownState::Completed => "completed",
    }
}

fn system_time_to_ms(timestamp: SystemTime) -> u128 {
    timestamp
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
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
    use crate::session::lifecycle::SessionLifecyclePayload;
    use crate::session::publisher::FocusWindowContext;
    use crate::session::publisher::PublisherError;
    use anyhow::anyhow;
    use async_trait::async_trait;
    use serde_json::json;
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};
    use tokio::sync::{broadcast::error::RecvError, Mutex as AsyncMutex};
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

    fn make_snapshot(session_id: &str, raw: &str, polished: &str) -> SessionSnapshot {
        SessionSnapshot {
            session_id: session_id.into(),
            started_at_ms: 0,
            completed_at_ms: 0,
            locale: None,
            app_identifier: Some("com.example.app".into()),
            app_version: None,
            confidence_score: None,
            raw_transcript: raw.into(),
            polished_transcript: polished.into(),
            metadata: json!({}),
            post_actions: vec![],
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
    async fn noise_warning_events_forwarded_to_session_channel() {
        let local_engine = Arc::new(ProgrammedSpeechEngine::new(vec![Ok(String::new())]));
        let orchestrator = EngineOrchestrator::with_engine(
            EngineConfig {
                prefer_cloud: false,
            },
            local_engine,
        );
        let manager = SessionManager::with_orchestrator(orchestrator);
        manager
            .run()
            .await
            .expect("session manager bootstrap should succeed");

        manager.set_active_session_id("session-manual-stop").await;

        manager
            .set_active_session_id("session-noise-telemetry")
            .await;

        let audio = manager.audio_pipeline();
        let mut events_rx = manager.subscribe_events();

        audio.begin_preroll(Some(-32.0));
        audio.begin_recording();

        let loud_frame = vec![0.8_f32; 1_600];
        for _ in 0..3 {
            audio
                .push_pcm_frame(loud_frame.clone())
                .await
                .expect("push loud frame");
        }

        let warning = timeout(Duration::from_millis(800), async {
            loop {
                match events_rx.recv().await {
                    Ok(SessionEvent::NoiseWarning(payload)) => break payload,
                    Ok(_) => continue,
                    Err(RecvError::Lagged(_)) => continue,
                    Err(err) => panic!("session event channel closed: {err:?}"),
                }
            }
        })
        .await
        .expect("noise warning timed out");

        assert!((warning.threshold_db - warning.baseline_db - 15.0).abs() < 0.5);
        assert!(warning.level_db >= warning.threshold_db);
        assert!(warning.persistence_ms >= 300);
    }

    #[tokio::test]
    async fn silence_countdown_completion_triggers_auto_stop_once() {
        let local_engine = Arc::new(ProgrammedSpeechEngine::new(vec![Ok(String::new())]));
        let orchestrator = EngineOrchestrator::with_engine(
            EngineConfig {
                prefer_cloud: false,
            },
            local_engine,
        );
        let manager = SessionManager::with_orchestrator(orchestrator);
        manager
            .run()
            .await
            .expect("session manager bootstrap should succeed");

        manager
            .set_active_session_id("session-silence-autostop")
            .await;

        let audio = manager.audio_pipeline();
        let mut events_rx = manager.subscribe_events();

        audio.begin_preroll(Some(-28.0));
        audio.begin_recording();

        let quiet_frame = vec![0.001_f32; 1_600];
        for _ in 0..50 {
            audio
                .push_pcm_frame(quiet_frame.clone())
                .await
                .expect("push quiet frame");
        }

        let mut countdown_completed = false;
        let mut auto_stop_count = 0;

        for _ in 0..128 {
            let event = timeout(Duration::from_millis(1_000), events_rx.recv())
                .await
                .expect("waiting for session event timed out");

            match event {
                Ok(SessionEvent::SilenceCountdown(payload))
                    if payload.state == SilenceCountdownState::Completed =>
                {
                    countdown_completed = true;
                }
                Ok(SessionEvent::AutoStop(_)) => {
                    auto_stop_count += 1;
                    break;
                }
                Ok(_) => {}
                Err(RecvError::Lagged(_)) => continue,
                Err(err) => panic!("session event channel closed: {err:?}"),
            }
        }

        assert!(countdown_completed, "expected countdown completion event");
        assert_eq!(auto_stop_count, 1, "auto-stop should trigger exactly once");

        for _ in 0..5 {
            audio
                .push_pcm_frame(quiet_frame.clone())
                .await
                .expect("push quiet frame after auto-stop");
        }

        if let Ok(event) = timeout(Duration::from_millis(250), events_rx.recv()).await {
            if let Ok(event) = event {
                assert!(
                    !matches!(event, SessionEvent::AutoStop(_)),
                    "unexpected extra auto-stop event",
                );
            }
        }
    }

    #[tokio::test]
    async fn manual_stop_cancels_active_silence_countdown() {
        let local_engine = Arc::new(ProgrammedSpeechEngine::new(vec![Ok(String::new())]));
        let orchestrator = EngineOrchestrator::with_engine(
            EngineConfig {
                prefer_cloud: false,
            },
            local_engine,
        );
        let manager = SessionManager::with_orchestrator(orchestrator);
        manager
            .run()
            .await
            .expect("session manager bootstrap should succeed");

        manager.set_active_session_id("session-manual-stop").await;

        let audio = manager.audio_pipeline();
        let mut events_rx = manager.subscribe_events();

        audio.begin_preroll(Some(-30.0));
        audio.begin_recording();

        let quiet_frame = vec![0.001_f32; 1_600];
        audio
            .push_pcm_frame(quiet_frame.clone())
            .await
            .expect("push quiet frame");

        timeout(Duration::from_millis(800), async {
            loop {
                match events_rx.recv().await {
                    Ok(SessionEvent::SilenceCountdown(payload))
                        if payload.state == SilenceCountdownState::Started =>
                    {
                        break;
                    }
                    Ok(_) => continue,
                    Err(RecvError::Lagged(_)) => continue,
                    Err(err) => panic!("session event channel closed: {err:?}"),
                }
            }
        })
        .await
        .expect("countdown start not observed");

        manager.cancel_silence_countdown_due_to_manual_stop().await;

        let canceled = timeout(Duration::from_millis(800), async {
            loop {
                match events_rx.recv().await {
                    Ok(SessionEvent::SilenceCountdown(payload))
                        if payload.state == SilenceCountdownState::Canceled =>
                    {
                        break payload;
                    }
                    Ok(_) => continue,
                    Err(RecvError::Lagged(_)) => continue,
                    Err(err) => panic!("session event channel closed: {err:?}"),
                }
            }
        })
        .await
        .expect("manual cancellation event missing");

        assert_eq!(
            canceled.cancel_reason,
            Some(SilenceCancellationReason::ManualStop),
            "expected manual stop cancel reason",
        );

        audio.reset_session();
        audio.begin_preroll(Some(-30.0));
        audio.begin_recording();

        for _ in 0..10 {
            audio
                .push_pcm_frame(quiet_frame.clone())
                .await
                .expect("push quiet frame after restart");
        }

        timeout(Duration::from_millis(1_200), async {
            loop {
                match events_rx.recv().await {
                    Ok(SessionEvent::SilenceCountdown(payload))
                        if payload.state == SilenceCountdownState::Started =>
                    {
                        break;
                    }
                    Ok(_) => continue,
                    Err(RecvError::Lagged(_)) => continue,
                    Err(err) => panic!("session event channel closed: {err:?}"),
                }
            }
        })
        .await
        .expect("countdown did not restart after manual cancel");
    }

    #[tokio::test]
    async fn noise_warning_is_persisted_to_telemetry_queue() {
        let local_engine = Arc::new(ProgrammedSpeechEngine::new(vec![Ok(String::new())]));
        let orchestrator = EngineOrchestrator::with_engine(
            EngineConfig {
                prefer_cloud: false,
            },
            local_engine,
        );
        let manager = SessionManager::with_orchestrator(orchestrator);
        manager
            .run()
            .await
            .expect("session manager bootstrap should succeed");

        manager
            .set_active_session_id("session-telemetry-noise")
            .await;

        let audio = manager.audio_pipeline();
        audio.begin_preroll(Some(-36.0));
        audio.begin_recording();

        let loud_frame = vec![0.9_f32; 1_600];
        for _ in 0..3 {
            audio
                .push_pcm_frame(loud_frame.clone())
                .await
                .expect("push loud frame");
        }

        let mut rx = manager.subscribe_events();
        timeout(Duration::from_millis(800), async {
            loop {
                match rx.recv().await {
                    Ok(SessionEvent::NoiseWarning(_)) => break,
                    Ok(_) => continue,
                    Err(RecvError::Lagged(_)) => continue,
                    Err(err) => panic!("session event channel closed: {err:?}"),
                }
            }
        })
        .await
        .expect("noise warning timed out");

        let persistence = manager.persistence_handle();
        let conn = persistence
            .sqlite()
            .connection()
            .expect("persistence connection");
        let (event_type, payload): (String, String) = conn
            .query_row(
                "SELECT event_type, payload FROM telemetry_queue ORDER BY id DESC LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("telemetry row");

        assert_eq!(event_type, EVENT_NOISE_WARNING);
        let payload_json: serde_json::Value =
            serde_json::from_str(&payload).expect("noise telemetry payload");
        assert_eq!(payload_json["sessionId"], "session-telemetry-noise");
        assert!(payload_json["thresholdDb"].as_f64().is_some());
        assert!(payload_json["occurredAtMs"].as_u64().is_some());
    }

    #[tokio::test]
    async fn silence_completion_records_auto_stop_telemetry() {
        let local_engine = Arc::new(ProgrammedSpeechEngine::new(vec![Ok(String::new())]));
        let orchestrator = EngineOrchestrator::with_engine(
            EngineConfig {
                prefer_cloud: false,
            },
            local_engine,
        );
        let manager = SessionManager::with_orchestrator(orchestrator);
        manager
            .run()
            .await
            .expect("session manager bootstrap should succeed");

        manager
            .set_active_session_id("session-telemetry-silence")
            .await;

        let audio = manager.audio_pipeline();
        audio.begin_preroll(Some(-28.0));
        audio.begin_recording();

        let quiet_frame = vec![0.0005_f32; 1_600];
        for _ in 0..60 {
            audio
                .push_pcm_frame(quiet_frame.clone())
                .await
                .expect("push quiet frame");
        }

        let mut rx = manager.subscribe_events();
        timeout(Duration::from_secs(3), async {
            loop {
                match rx.recv().await {
                    Ok(SessionEvent::AutoStop(_)) => break,
                    Ok(_) => continue,
                    Err(RecvError::Lagged(_)) => continue,
                    Err(err) => panic!("session event channel closed: {err:?}"),
                }
            }
        })
        .await
        .expect("auto-stop timed out");

        let persistence = manager.persistence_handle();
        let conn = persistence
            .sqlite()
            .connection()
            .expect("persistence connection");
        let (event_type, payload): (String, String) = conn
            .query_row(
                "SELECT event_type, payload FROM telemetry_queue ORDER BY id DESC LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("telemetry row");

        assert_eq!(event_type, EVENT_SILENCE_AUTOSTOP);
        let payload_json: serde_json::Value =
            serde_json::from_str(&payload).expect("auto-stop telemetry payload");
        assert_eq!(payload_json["sessionId"], "session-telemetry-silence");
        assert_eq!(payload_json["reason"], "silenceTimeout");
        assert!(payload_json["countdownMs"].as_u64().is_some());
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
        let snapshot = make_snapshot("session-1", "raw", "polished");
        let request = PublishRequest {
            transcript: "polished".into(),
            focus: FocusWindowContext::from_app_identifier("com.example.app"),
            fallback: FallbackStrategy::ClipboardCopy,
        };

        let outcome = manager
            .publish_transcript(snapshot, request)
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
        let snapshot = make_snapshot("session-2", "raw", "");
        let request = PublishRequest {
            transcript: "   ".into(),
            focus: FocusWindowContext::default(),
            fallback: FallbackStrategy::NotifyOnly,
        };

        let result = manager.publish_transcript(snapshot, request).await;
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
        let snapshot = make_snapshot("session-fallback", "raw", "polished");
        let request = PublishRequest {
            transcript: "polished".into(),
            focus: FocusWindowContext::default(),
            fallback: FallbackStrategy::ClipboardCopy,
        };

        let outcome = manager
            .publish_transcript(snapshot, request)
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
        let snapshot = make_snapshot("session-fallback-fail", "raw", "polished");
        let request = PublishRequest {
            transcript: "polished".into(),
            focus: FocusWindowContext::default(),
            fallback: FallbackStrategy::ClipboardCopy,
        };

        let outcome = manager
            .publish_transcript(snapshot, request)
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
