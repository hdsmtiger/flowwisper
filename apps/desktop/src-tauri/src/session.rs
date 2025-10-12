use flowwisper_core::session::{
    AutoStopReason as CoreAutoStopReason, SessionEvent as CoreSessionEvent,
    SessionNoiseWarning as CoreSessionNoiseWarning,
    SessionSilenceCountdown as CoreSessionSilenceCountdown,
    SilenceCancellationReason as CoreSilenceCancellationReason,
    SilenceCountdownState as CoreSilenceCountdownState,
};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};
use tauri::{AppHandle, Emitter};

const TRANSCRIPT_EVENT_CHANNEL: &str = "session://transcript";
const LIFECYCLE_EVENT_CHANNEL: &str = "session://lifecycle";
const PUBLISH_RESULT_CHANNEL: &str = "session://publish-result";
const PUBLISH_NOTICE_CHANNEL: &str = "session://publish-notice";
const SESSION_EVENT_CHANNEL: &str = "session://event";
const MAX_TRANSCRIPT_HISTORY: usize = 120;
const MAX_COMPLETION_HISTORY: usize = 120;
const MAX_SESSION_EVENT_HISTORY: usize = 120;

fn current_timestamp_ms() -> u128 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionStatus {
    pub phase: String,
    pub detail: String,
    pub timestamp_ms: u128,
}

impl SessionStatus {
    fn new(phase: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            phase: phase.into(),
            detail: detail.into(),
            timestamp_ms: current_timestamp_ms(),
        }
    }
}

impl Default for SessionStatus {
    fn default() -> Self {
        Self::new(
            "Idle",
            "Core service bridge not connected — awaiting initialization",
        )
    }
}

#[derive(Debug, Clone)]
pub struct SessionStateManager {
    current: Arc<Mutex<SessionStatus>>,
    history: Arc<Mutex<Vec<SessionStatus>>>,
    transcript_history: Arc<Mutex<VecDeque<TranscriptStreamEvent>>>,
    publishing_history: Arc<Mutex<VecDeque<PublishingUpdate>>>,
    insertion_history: Arc<Mutex<VecDeque<InsertionResult>>>,
    notice_history: Arc<Mutex<VecDeque<PublishNotice>>>,
    event_history: Arc<Mutex<VecDeque<SessionRealtimeEvent>>>,
}

impl SessionStateManager {
    pub fn new() -> Self {
        Self {
            current: Arc::new(Mutex::new(SessionStatus::default())),
            history: Arc::new(Mutex::new(vec![SessionStatus::default()])),
            transcript_history: Arc::new(Mutex::new(VecDeque::new())),
            publishing_history: Arc::new(Mutex::new(VecDeque::new())),
            insertion_history: Arc::new(Mutex::new(VecDeque::new())),
            notice_history: Arc::new(Mutex::new(VecDeque::new())),
            event_history: Arc::new(Mutex::new(VecDeque::new())),
        }
    }

    pub fn snapshot(&self) -> Result<SessionStatus, String> {
        self.current
            .lock()
            .map(|status| status.clone())
            .map_err(|err| format!("failed to read session status: {err}"))
    }

    pub fn timeline(&self) -> Result<Vec<SessionStatus>, String> {
        self.history
            .lock()
            .map(|history| history.clone())
            .map_err(|err| format!("failed to read session timeline: {err}"))
    }

    pub fn transition(
        &self,
        phase: impl Into<String>,
        detail: impl Into<String>,
    ) -> Result<SessionStatus, String> {
        let updated = SessionStatus::new(phase, detail);
        {
            let mut guard = self
                .current
                .lock()
                .map_err(|err| format!("failed to update session status: {err}"))?;
            *guard = updated.clone();
        }

        let mut history = self
            .history
            .lock()
            .map_err(|err| format!("failed to update session timeline: {err}"))?;
        history.push(updated.clone());
        if history.len() > 50 {
            let drain = history.len() - 50;
            history.drain(0..drain);
        }
        Ok(updated)
    }

    pub fn transition_and_emit(
        &self,
        app: &AppHandle,
        phase: impl Into<String>,
        detail: impl Into<String>,
    ) -> Result<SessionStatus, String> {
        let status = self.transition(phase, detail)?;
        app.emit("session://state", &status)
            .map_err(|err| format!("failed to emit session state: {err}"))?;
        Ok(status)
    }

    pub fn drive_preroll(
        &self,
        app: &AppHandle,
        priming_detail: impl Into<String>,
        preroll_detail: impl Into<String>,
    ) {
        let manager = self.clone();
        let handle = app.clone();
        let priming = priming_detail.into();
        let preroll = preroll_detail.into();
        let send_preroll = !preroll.trim().is_empty();
        std::thread::spawn(move || {
            let _ = manager.transition_and_emit(&handle, "Priming", priming);
            if send_preroll {
                std::thread::sleep(Duration::from_millis(120));
                let _ = manager.transition_and_emit(&handle, "PreRoll", preroll);
            }
        });
    }

    pub fn mark_processing(&self, app: AppHandle) {
        let manager = self.clone();
        std::thread::spawn(move || {
            let _ =
                manager.transition_and_emit(&app, "Processing", "Awaiting core-service handshake");
        });
    }

    pub fn complete_ready(&self, app: AppHandle) {
        let _ = self.transition_and_emit(&app, "Ready", "Session ready for hands-free capture");
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TranscriptSourceVariant {
    Raw,
    Polished,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TranscriptStreamSource {
    Local,
    Cloud,
    Polished,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TranscriptNoticeLevel {
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptSentence {
    pub sentence_id: u64,
    pub text: String,
    pub source: TranscriptStreamSource,
    pub is_primary: bool,
    pub within_sla: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptSentenceSelection {
    pub sentence_id: u64,
    pub active_variant: TranscriptSourceVariant,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptNotice {
    pub level: TranscriptNoticeLevel,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum TranscriptStreamPayload {
    Transcript {
        sentence: TranscriptSentence,
    },
    Notice {
        notice: TranscriptNotice,
    },
    Selection {
        selections: Vec<TranscriptSentenceSelection>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptStreamEvent {
    pub timestamp_ms: u128,
    pub frame_index: u64,
    pub latency_ms: u64,
    pub is_first: bool,
    pub payload: TranscriptStreamPayload,
}

impl TranscriptStreamEvent {
    pub fn new(
        frame_index: u64,
        latency_ms: u64,
        is_first: bool,
        payload: TranscriptStreamPayload,
    ) -> Self {
        Self {
            timestamp_ms: current_timestamp_ms(),
            frame_index,
            latency_ms,
            is_first,
            payload,
        }
    }
}

impl SessionStateManager {
    fn retain_with_limit<T>(history: &mut VecDeque<T>, event: T, limit: usize) {
        history.push_back(event);
        while history.len() > limit {
            history.pop_front();
        }
    }

    fn retain_transcript_history(
        history: &mut VecDeque<TranscriptStreamEvent>,
        event: TranscriptStreamEvent,
    ) {
        Self::retain_with_limit(history, event, MAX_TRANSCRIPT_HISTORY);
    }

    fn retain_session_events(
        history: &mut VecDeque<SessionRealtimeEvent>,
        event: SessionRealtimeEvent,
    ) {
        Self::retain_with_limit(history, event, MAX_SESSION_EVENT_HISTORY);
    }

    pub fn record_transcript_event(&self, event: TranscriptStreamEvent) -> Result<(), String> {
        let mut history = self
            .transcript_history
            .lock()
            .map_err(|err| format!("failed to update transcript history: {err}"))?;
        Self::retain_transcript_history(&mut history, event);
        Ok(())
    }

    pub fn emit_transcript_event(
        &self,
        app: &AppHandle,
        event: TranscriptStreamEvent,
    ) -> Result<(), String> {
        self.record_transcript_event(event.clone())?;
        app.emit(TRANSCRIPT_EVENT_CHANNEL, &event)
            .map_err(|err| format!("failed to emit transcript event: {err}"))
    }

    pub fn apply_transcript_selection(
        &self,
        app: &AppHandle,
        selections: Vec<TranscriptSentenceSelection>,
    ) -> Result<(), String> {
        if selections.is_empty() {
            return Ok(());
        }

        let event = TranscriptStreamEvent::new(
            0,
            0,
            false,
            TranscriptStreamPayload::Selection { selections },
        );
        self.emit_transcript_event(app, event)
    }

    pub fn transcript_log(&self) -> Result<Vec<TranscriptStreamEvent>, String> {
        let history = self
            .transcript_history
            .lock()
            .map_err(|err| format!("failed to read transcript history: {err}"))?;
        Ok(history.iter().cloned().collect())
    }

    pub fn record_session_event(&self, event: SessionRealtimeEvent) -> Result<(), String> {
        let mut history = self
            .event_history
            .lock()
            .map_err(|err| format!("failed to record session event: {err}"))?;
        Self::retain_session_events(&mut history, event);
        Ok(())
    }

    pub fn emit_session_event(
        &self,
        app: &AppHandle,
        event: SessionRealtimeEvent,
    ) -> Result<(), String> {
        event.validate()?;
        self.record_session_event(event.clone())?;
        app.emit(SESSION_EVENT_CHANNEL, &event)
            .map_err(|err| format!("failed to emit session event: {err}"))
    }

    pub fn emit_core_session_event(
        &self,
        app: &AppHandle,
        event: CoreSessionEvent,
    ) -> Result<(), String> {
        let transformed = SessionRealtimeEvent::from_core_event(event);
        self.emit_session_event(app, transformed)
    }

    pub fn session_event_history(&self) -> Result<Vec<SessionRealtimeEvent>, String> {
        let history = self
            .event_history
            .lock()
            .map_err(|err| format!("failed to read session event history: {err}"))?;
        Ok(history.iter().cloned().collect())
    }

    fn record_publishing_update(&self, update: PublishingUpdate) -> Result<(), String> {
        let mut history = self
            .publishing_history
            .lock()
            .map_err(|err| format!("failed to record publishing update: {err}"))?;
        Self::retain_with_limit(&mut history, update, MAX_COMPLETION_HISTORY);
        Ok(())
    }

    pub fn emit_publishing_update(
        &self,
        app: &AppHandle,
        update: PublishingUpdate,
    ) -> Result<(), String> {
        self.record_publishing_update(update.clone())?;
        app.emit(LIFECYCLE_EVENT_CHANNEL, &update)
            .map_err(|err| format!("failed to emit publishing update: {err}"))
    }

    pub fn publishing_history(&self) -> Result<Vec<PublishingUpdate>, String> {
        let history = self
            .publishing_history
            .lock()
            .map_err(|err| format!("failed to read publishing history: {err}"))?;
        Ok(history.iter().cloned().collect())
    }

    fn record_insertion_result(&self, result: InsertionResult) -> Result<(), String> {
        let mut history = self
            .insertion_history
            .lock()
            .map_err(|err| format!("failed to record insertion result: {err}"))?;
        Self::retain_with_limit(&mut history, result, MAX_COMPLETION_HISTORY);
        Ok(())
    }

    pub fn emit_insertion_result(
        &self,
        app: &AppHandle,
        result: InsertionResult,
    ) -> Result<(), String> {
        self.record_insertion_result(result.clone())?;
        app.emit(PUBLISH_RESULT_CHANNEL, &result)
            .map_err(|err| format!("failed to emit insertion result: {err}"))
    }

    pub fn insertion_history(&self) -> Result<Vec<InsertionResult>, String> {
        let history = self
            .insertion_history
            .lock()
            .map_err(|err| format!("failed to read insertion history: {err}"))?;
        Ok(history.iter().cloned().collect())
    }

    fn record_publish_notice(&self, notice: PublishNotice) -> Result<(), String> {
        let mut history = self
            .notice_history
            .lock()
            .map_err(|err| format!("failed to record publish notice: {err}"))?;
        Self::retain_with_limit(&mut history, notice, MAX_COMPLETION_HISTORY);
        Ok(())
    }

    pub fn emit_publish_notice(
        &self,
        app: &AppHandle,
        notice: PublishNotice,
    ) -> Result<(), String> {
        self.record_publish_notice(notice.clone())?;
        app.emit(PUBLISH_NOTICE_CHANNEL, &notice)
            .map_err(|err| format!("failed to emit publish notice: {err}"))
    }

    pub fn publish_notice_history(&self) -> Result<Vec<PublishNotice>, String> {
        let history = self
            .notice_history
            .lock()
            .map_err(|err| format!("failed to read publish notice history: {err}"))?;
        Ok(history.iter().cloned().collect())
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum SessionSilenceCountdownState {
    Started,
    Tick,
    Canceled,
    Completed,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum SessionSilenceCancellationReason {
    SpeechDetected,
    ManualStop,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum SessionAutoStopReason {
    SilenceTimeout,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum SessionRealtimeEvent {
    NoiseWarning {
        timestamp_ms: u128,
        baseline_db: f32,
        threshold_db: f32,
        level_db: f32,
        persistence_ms: u32,
    },
    SilenceCountdown {
        timestamp_ms: u128,
        total_ms: u32,
        remaining_ms: u32,
        state: SessionSilenceCountdownState,
        cancel_reason: Option<SessionSilenceCancellationReason>,
    },
    AutoStop {
        timestamp_ms: u128,
        reason: SessionAutoStopReason,
    },
}

impl SessionRealtimeEvent {
    fn validate(&self) -> Result<(), String> {
        match self {
            SessionRealtimeEvent::NoiseWarning {
                baseline_db,
                threshold_db,
                level_db,
                persistence_ms,
                ..
            } => {
                if !baseline_db.is_finite() || !threshold_db.is_finite() || !level_db.is_finite() {
                    return Err("noise warning event contains non-finite levels".into());
                }
                if *persistence_ms == 0 {
                    return Err("noise warning persistence must be positive".into());
                }
            }
            SessionRealtimeEvent::SilenceCountdown {
                total_ms,
                remaining_ms,
                state,
                cancel_reason,
                ..
            } => {
                if *total_ms == 0 {
                    return Err("silence countdown total must be positive".into());
                }
                if remaining_ms > total_ms {
                    return Err("silence countdown remaining exceeds total".into());
                }
                if matches!(state, SessionSilenceCountdownState::Completed) && *remaining_ms != 0 {
                    return Err("completed silence countdown must report 0 remaining".into());
                }
                if matches!(state, SessionSilenceCountdownState::Canceled) {
                    if cancel_reason.is_none() {
                        return Err("canceled silence countdown must include a reason".into());
                    }
                } else if cancel_reason.is_some() {
                    return Err(
                        "silence countdown cancel reason only allowed when state is canceled"
                            .into(),
                    );
                }
            }
            SessionRealtimeEvent::AutoStop { .. } => {}
        }

        Ok(())
    }

    fn from_core_event(event: CoreSessionEvent) -> Self {
        match event {
            CoreSessionEvent::NoiseWarning(payload) => SessionRealtimeEvent::NoiseWarning {
                timestamp_ms: current_timestamp_ms(),
                baseline_db: payload.baseline_db,
                threshold_db: payload.threshold_db,
                level_db: payload.level_db,
                persistence_ms: payload.persistence_ms,
            },
            CoreSessionEvent::SilenceCountdown(payload) => SessionRealtimeEvent::SilenceCountdown {
                timestamp_ms: current_timestamp_ms(),
                total_ms: payload.total_ms,
                remaining_ms: payload.remaining_ms,
                state: payload.state.into(),
                cancel_reason: payload.cancel_reason.map(Into::into),
            },
            CoreSessionEvent::AutoStop(payload) => SessionRealtimeEvent::AutoStop {
                timestamp_ms: current_timestamp_ms(),
                reason: payload.reason.into(),
            },
        }
    }
}

impl From<CoreSilenceCountdownState> for SessionSilenceCountdownState {
    fn from(value: CoreSilenceCountdownState) -> Self {
        match value {
            CoreSilenceCountdownState::Started => SessionSilenceCountdownState::Started,
            CoreSilenceCountdownState::Tick => SessionSilenceCountdownState::Tick,
            CoreSilenceCountdownState::Canceled => SessionSilenceCountdownState::Canceled,
            CoreSilenceCountdownState::Completed => SessionSilenceCountdownState::Completed,
        }
    }
}

impl From<CoreSilenceCancellationReason> for SessionSilenceCancellationReason {
    fn from(value: CoreSilenceCancellationReason) -> Self {
        match value {
            CoreSilenceCancellationReason::SpeechDetected => {
                SessionSilenceCancellationReason::SpeechDetected
            }
            CoreSilenceCancellationReason::ManualStop => {
                SessionSilenceCancellationReason::ManualStop
            }
        }
    }
}

impl From<CoreAutoStopReason> for SessionAutoStopReason {
    fn from(value: CoreAutoStopReason) -> Self {
        match value {
            CoreAutoStopReason::SilenceTimeout => SessionAutoStopReason::SilenceTimeout,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum PublishStrategy {
    DirectInsert,
    ClipboardFallback,
    NotifyOnly,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum FallbackStrategy {
    ClipboardCopy,
    NotifyOnly,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum PublishStatus {
    Completed,
    Deferred,
    Failed,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PublishNoticeLevel {
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum PublishActionKind {
    Insert,
    Copy,
    SaveDraft,
    UndoPrompt,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PublishingUpdate {
    pub session_id: String,
    pub attempt: u8,
    pub strategy: PublishStrategy,
    pub fallback: Option<FallbackStrategy>,
    pub retrying: bool,
    pub detail: Option<String>,
    pub timestamp_ms: u128,
}

impl PublishingUpdate {
    pub fn new(
        session_id: impl Into<String>,
        attempt: u8,
        strategy: PublishStrategy,
        fallback: Option<FallbackStrategy>,
        retrying: bool,
        detail: Option<String>,
    ) -> Self {
        Self {
            session_id: session_id.into(),
            attempt,
            strategy,
            fallback,
            retrying,
            detail,
            timestamp_ms: current_timestamp_ms(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct InsertionFailure {
    pub code: Option<String>,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct InsertionResult {
    pub session_id: String,
    pub status: PublishStatus,
    pub strategy: PublishStrategy,
    pub attempts: u8,
    pub fallback: Option<FallbackStrategy>,
    pub failure: Option<InsertionFailure>,
    pub undo_token: Option<String>,
    pub timestamp_ms: u128,
}

impl InsertionResult {
    pub fn new(
        session_id: impl Into<String>,
        status: PublishStatus,
        strategy: PublishStrategy,
        attempts: u8,
        fallback: Option<FallbackStrategy>,
        failure: Option<InsertionFailure>,
        undo_token: Option<String>,
    ) -> Self {
        Self {
            session_id: session_id.into(),
            status,
            strategy,
            attempts,
            fallback,
            failure,
            undo_token,
            timestamp_ms: current_timestamp_ms(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PublishNotice {
    pub session_id: String,
    pub action: PublishActionKind,
    pub level: PublishNoticeLevel,
    pub message: String,
    pub undo_token: Option<String>,
    pub timestamp_ms: u128,
}

impl PublishNotice {
    pub fn new(
        session_id: impl Into<String>,
        action: PublishActionKind,
        level: PublishNoticeLevel,
        message: impl Into<String>,
        undo_token: Option<String>,
    ) -> Self {
        Self {
            session_id: session_id.into(),
            action,
            level,
            message: message.into(),
            undo_token,
            timestamp_ms: current_timestamp_ms(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retains_latest_transcript_events() {
        let manager = SessionStateManager::new();

        for idx in 0..130u64 {
            let mut event = TranscriptStreamEvent::new(
                idx,
                42,
                idx == 0,
                TranscriptStreamPayload::Transcript {
                    sentence: TranscriptSentence {
                        sentence_id: idx,
                        text: format!("Sentence #{idx}"),
                        source: TranscriptStreamSource::Local,
                        is_primary: true,
                        within_sla: true,
                    },
                },
            );
            event.timestamp_ms = idx as u128;
            manager
                .record_transcript_event(event)
                .expect("record event");
        }

        let log = manager.transcript_log().expect("read transcript log");
        assert_eq!(log.len(), MAX_TRANSCRIPT_HISTORY);
        assert_eq!(log.first().unwrap().frame_index, 10);
        assert_eq!(log.last().unwrap().frame_index, 129);
    }

    #[test]
    fn transcript_event_new_populates_timestamp() {
        let payload = TranscriptStreamPayload::Notice {
            notice: TranscriptNotice {
                level: TranscriptNoticeLevel::Info,
                message: "ok".into(),
            },
        };
        let event = TranscriptStreamEvent::new(7, 15, false, payload);
        assert!(event.timestamp_ms > 0);
        assert_eq!(event.frame_index, 7);
        assert_eq!(event.latency_ms, 15);
        assert!(!event.is_first);
    }

    #[test]
    fn publishing_history_retains_latest_updates() {
        let manager = SessionStateManager::new();

        for idx in 0..(MAX_COMPLETION_HISTORY + 10) as u8 {
            let update = PublishingUpdate::new(
                "session",
                idx,
                PublishStrategy::DirectInsert,
                Some(FallbackStrategy::ClipboardCopy),
                idx % 2 == 0,
                Some(format!("attempt #{idx}")),
            );
            manager
                .record_publishing_update(update)
                .expect("record publishing update");
        }

        let history = manager
            .publishing_history()
            .expect("read publishing history");
        assert_eq!(history.len(), MAX_COMPLETION_HISTORY);
        assert_eq!(history.first().unwrap().attempt as usize, 10);
        assert!(history.last().unwrap().timestamp_ms > 0);
    }

    #[test]
    fn insertion_history_retains_latest_results() {
        let manager = SessionStateManager::new();

        for idx in 0..(MAX_COMPLETION_HISTORY + 5) as u8 {
            let result = InsertionResult::new(
                "session",
                PublishStatus::Failed,
                PublishStrategy::DirectInsert,
                idx,
                Some(FallbackStrategy::NotifyOnly),
                Some(InsertionFailure {
                    code: Some("timeout".into()),
                    message: format!("attempt {idx} timed out"),
                }),
                Some("undo".into()),
            );
            manager
                .record_insertion_result(result)
                .expect("record insertion result");
        }

        let history = manager.insertion_history().expect("read insertion history");
        assert_eq!(history.len(), MAX_COMPLETION_HISTORY);
        assert_eq!(history.first().unwrap().attempts as usize, 5);
        assert!(history.iter().any(|entry| entry.failure.is_some()));
    }

    #[test]
    fn publish_notice_history_retains_latest_entries() {
        let manager = SessionStateManager::new();

        for idx in 0..(MAX_COMPLETION_HISTORY + 3) as u8 {
            let notice = PublishNotice::new(
                "session",
                PublishActionKind::Copy,
                if idx % 2 == 0 {
                    PublishNoticeLevel::Info
                } else {
                    PublishNoticeLevel::Warn
                },
                format!("notice {idx}"),
                None,
            );
            manager
                .record_publish_notice(notice)
                .expect("record publish notice");
        }

        let history = manager
            .publish_notice_history()
            .expect("read publish notice history");
        assert_eq!(history.len(), MAX_COMPLETION_HISTORY);
        assert_eq!(history.first().unwrap().message, "notice 3");
        assert!(history
            .iter()
            .any(|entry| entry.level == PublishNoticeLevel::Warn));
    }

    #[test]
    fn session_event_history_retains_latest_entries() {
        let manager = SessionStateManager::new();

        for idx in 0..(MAX_SESSION_EVENT_HISTORY + 5) as u32 {
            let event = SessionRealtimeEvent::NoiseWarning {
                timestamp_ms: idx as u128,
                baseline_db: 30.0,
                threshold_db: 45.0,
                level_db: 60.0,
                persistence_ms: 300,
            };
            manager
                .record_session_event(event)
                .expect("record session event");
        }

        let history = manager
            .session_event_history()
            .expect("read session event history");
        assert_eq!(history.len(), MAX_SESSION_EVENT_HISTORY);
        assert!(matches!(
            history.last().unwrap(),
            SessionRealtimeEvent::NoiseWarning { level_db, .. } if (*level_db - 60.0).abs() < f32::EPSILON
        ));
    }

    #[test]
    fn session_realtime_event_validation_rejects_invalid_countdown() {
        let invalid = SessionRealtimeEvent::SilenceCountdown {
            timestamp_ms: 1,
            total_ms: 5000,
            remaining_ms: 6000,
            state: SessionSilenceCountdownState::Tick,
            cancel_reason: None,
        };

        assert!(invalid.validate().is_err());

        let missing_reason = SessionRealtimeEvent::SilenceCountdown {
            timestamp_ms: 2,
            total_ms: 5000,
            remaining_ms: 2000,
            state: SessionSilenceCountdownState::Canceled,
            cancel_reason: None,
        };

        assert!(missing_reason.validate().is_err());
    }

    #[test]
    fn session_realtime_event_from_core_event_maps_fields() {
        let warning = CoreSessionNoiseWarning {
            baseline_db: 20.0,
            threshold_db: 35.0,
            level_db: 52.0,
            persistence_ms: 250,
        };
        let countdown = CoreSessionSilenceCountdown {
            total_ms: 5000,
            remaining_ms: 3000,
            state: CoreSilenceCountdownState::Started,
            cancel_reason: None,
        };
        let auto_stop = CoreAutoStopReason::SilenceTimeout;

        match SessionRealtimeEvent::from_core_event(CoreSessionEvent::NoiseWarning(warning)) {
            SessionRealtimeEvent::NoiseWarning {
                baseline_db,
                threshold_db,
                level_db,
                persistence_ms,
                ..
            } => {
                assert!((baseline_db - 20.0).abs() < f32::EPSILON);
                assert!((threshold_db - 35.0).abs() < f32::EPSILON);
                assert!((level_db - 52.0).abs() < f32::EPSILON);
                assert_eq!(persistence_ms, 250);
            }
            _ => panic!("expected noise warning"),
        }

        match SessionRealtimeEvent::from_core_event(CoreSessionEvent::SilenceCountdown(countdown)) {
            SessionRealtimeEvent::SilenceCountdown {
                total_ms,
                remaining_ms,
                state,
                cancel_reason,
                ..
            } => {
                assert_eq!(total_ms, 5000);
                assert_eq!(remaining_ms, 3000);
                assert_eq!(state, SessionSilenceCountdownState::Started);
                assert!(cancel_reason.is_none());
            }
            _ => panic!("expected silence countdown"),
        }

        match SessionRealtimeEvent::from_core_event(CoreSessionEvent::AutoStop(
            flowwisper_core::session::SessionAutoStop { reason: auto_stop },
        )) {
            SessionRealtimeEvent::AutoStop { reason, .. } => {
                assert_eq!(reason, SessionAutoStopReason::SilenceTimeout);
            }
            _ => panic!("expected auto-stop"),
        }
    }
}
