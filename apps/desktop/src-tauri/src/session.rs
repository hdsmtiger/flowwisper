use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};
use tauri::{AppHandle, Emitter};

const TRANSCRIPT_EVENT_CHANNEL: &str = "session://transcript";
const MAX_TRANSCRIPT_HISTORY: usize = 120;

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
}

impl SessionStateManager {
    pub fn new() -> Self {
        Self {
            current: Arc::new(Mutex::new(SessionStatus::default())),
            history: Arc::new(Mutex::new(vec![SessionStatus::default()])),
            transcript_history: Arc::new(Mutex::new(VecDeque::new())),
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
    fn retain_transcript_history(
        history: &mut VecDeque<TranscriptStreamEvent>,
        event: TranscriptStreamEvent,
    ) {
        history.push_back(event);
        while history.len() > MAX_TRANSCRIPT_HISTORY {
            history.pop_front();
        }
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
}
