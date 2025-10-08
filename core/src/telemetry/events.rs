use anyhow::Error;
use serde::Serialize;
use std::time::Duration;
use tracing::{info, warn};

pub(crate) const TARGET: &str = "telemetry::dual_view";
pub(crate) const EVENT_LATENCY: &str = "dual_view_latency";
pub(crate) const EVENT_REVERT: &str = "dual_view_revert";

pub(crate) const SESSION_TARGET: &str = "telemetry::session";
pub(crate) const EVENT_PUBLISH_ATTEMPT: &str = "session_publish_attempt";
pub(crate) const EVENT_PUBLISH_OUTCOME: &str = "session_publish_outcome";
pub(crate) const EVENT_PUBLISH_FAILURE: &str = "session_publish_failure";
pub(crate) const EVENT_PUBLISH_DEGRADATION: &str = "session_publish_degradation";
pub(crate) const EVENT_DRAFT_SAVE_SUCCESS: &str = "session_draft_save_success";
pub(crate) const EVENT_DRAFT_SAVE_FAILURE: &str = "session_draft_save_failure";
pub(crate) const EVENT_PUBLISH_UNDO: &str = "session_publish_undo";
pub(crate) const EVENT_HISTORY_PERSISTED: &str = "session_history_persisted";
pub(crate) const EVENT_HISTORY_PERSIST_FAILURE: &str = "session_history_persist_failure";
pub(crate) const EVENT_HISTORY_ACCURACY: &str = "session_history_accuracy";
pub(crate) const EVENT_HISTORY_ACTION: &str = "session_history_action";
pub(crate) const EVENT_HISTORY_CLEANUP: &str = "session_history_cleanup";

#[derive(Debug, Serialize)]
pub struct DualViewLatencyEvent {
    pub sentence_id: u64,
    pub variant: &'static str,
    pub source: &'static str,
    pub is_primary: bool,
    pub latency_ms: u64,
    pub within_sla: bool,
}

#[derive(Debug, Serialize, Clone)]
pub struct DualViewSelectionLog {
    pub sentence_id: u64,
    pub variant: &'static str,
}

#[derive(Debug, Serialize)]
pub struct DualViewRevertEvent {
    pub requested: Vec<DualViewSelectionLog>,
    pub applied: Vec<DualViewSelectionLog>,
}

#[derive(Debug, Serialize)]
pub struct SessionPublishAttemptEvent<'a> {
    pub session_id: &'a str,
    pub app_identifier: Option<&'a str>,
    pub window_title: Option<&'a str>,
    pub fallback: &'a str,
}

#[derive(Debug, Serialize)]
pub struct SessionPublishOutcomeEvent<'a> {
    pub session_id: &'a str,
    pub status: &'a str,
    pub strategy: &'a str,
    pub attempts: u8,
    pub fallback: Option<&'a str>,
}

#[derive(Debug, Serialize)]
pub struct SessionPublishFailureEvent<'a> {
    pub session_id: &'a str,
    pub error: &'a str,
    pub attempts: u8,
    pub fallback: Option<&'a str>,
}

#[derive(Debug, Serialize)]
pub struct SessionPublishDegradationEvent<'a> {
    pub session_id: &'a str,
    pub fallback: &'a str,
    pub outcome: &'a str,
}

#[derive(Debug, Serialize)]
pub struct SessionDraftSaveEvent<'a> {
    pub session_id: &'a str,
    pub draft_id: &'a str,
    pub tags: Vec<&'a str>,
}

#[derive(Debug, Serialize)]
pub struct SessionDraftSaveFailureEvent<'a> {
    pub session_id: &'a str,
    pub error: &'a str,
}

#[derive(Debug, Serialize)]
pub struct SessionPublishUndoEvent<'a> {
    pub session_id: &'a str,
    pub undo_token: Option<&'a str>,
    pub origin: &'a str,
}

pub fn record_dual_view_latency(
    sentence_id: u64,
    variant: &'static str,
    source: &'static str,
    is_primary: bool,
    latency: Duration,
    within_sla: bool,
) {
    let event = DualViewLatencyEvent {
        sentence_id,
        variant,
        source,
        is_primary,
        latency_ms: duration_to_ms(latency),
        within_sla,
    };

    match serde_json::to_string(&event) {
        Ok(payload) => info!(
            target: TARGET,
            event = EVENT_LATENCY,
            sentence_id = event.sentence_id,
            variant = event.variant,
            source = event.source,
            is_primary = event.is_primary,
            latency_ms = event.latency_ms,
            within_sla = event.within_sla,
            payload = %payload
        ),
        Err(err) => warn!(
            target: TARGET,
            event = EVENT_LATENCY,
            %err,
            "failed to encode dual view latency event"
        ),
    }
}

pub fn record_dual_view_revert(
    requested: Vec<DualViewSelectionLog>,
    applied: Vec<DualViewSelectionLog>,
) {
    let requested_count = requested.len();
    let applied_count = applied.len();
    let event = DualViewRevertEvent { requested, applied };

    match serde_json::to_string(&event) {
        Ok(payload) => info!(
            target: TARGET,
            event = EVENT_REVERT,
            requested_count,
            applied_count,
            payload = %payload
        ),
        Err(err) => warn!(
            target: TARGET,
            event = EVENT_REVERT,
            %err,
            "failed to encode dual view revert event"
        ),
    }
}

pub fn record_session_publish_attempt(
    session_id: &str,
    app_identifier: Option<&str>,
    window_title: Option<&str>,
    fallback: &str,
) {
    let event = SessionPublishAttemptEvent {
        session_id,
        app_identifier,
        window_title,
        fallback,
    };

    match serde_json::to_string(&event) {
        Ok(payload) => info!(
            target: SESSION_TARGET,
            event = EVENT_PUBLISH_ATTEMPT,
            session_id,
            app_identifier,
            window_title,
            fallback,
            payload = %payload
        ),
        Err(err) => warn!(
            target: SESSION_TARGET,
            event = EVENT_PUBLISH_ATTEMPT,
            %err,
            "failed to encode session publish attempt"
        ),
    }
}

pub fn record_session_publish_outcome(
    session_id: &str,
    status: &str,
    strategy: &str,
    attempts: u8,
    fallback: Option<&str>,
) {
    let event = SessionPublishOutcomeEvent {
        session_id,
        status,
        strategy,
        attempts,
        fallback,
    };

    match serde_json::to_string(&event) {
        Ok(payload) => info!(
            target: SESSION_TARGET,
            event = EVENT_PUBLISH_OUTCOME,
            session_id,
            status,
            strategy,
            attempts,
            fallback,
            payload = %payload
        ),
        Err(err) => warn!(
            target: SESSION_TARGET,
            event = EVENT_PUBLISH_OUTCOME,
            %err,
            "failed to encode session publish outcome"
        ),
    }
}

pub fn record_session_publish_failure(
    session_id: &str,
    error: String,
    attempts: u8,
    fallback: Option<&str>,
) {
    let event = SessionPublishFailureEvent {
        session_id,
        error: &error,
        attempts,
        fallback,
    };

    match serde_json::to_string(&event) {
        Ok(payload) => warn!(
            target: SESSION_TARGET,
            event = EVENT_PUBLISH_FAILURE,
            session_id,
            attempts,
            fallback,
            error = %error,
            payload = %payload
        ),
        Err(err) => warn!(
            target: SESSION_TARGET,
            event = EVENT_PUBLISH_FAILURE,
            %err,
            "failed to encode session publish failure"
        ),
    }
}

pub fn record_session_publish_degradation(session_id: &str, fallback: &str, outcome: &str) {
    let event = SessionPublishDegradationEvent {
        session_id,
        fallback,
        outcome,
    };

    match serde_json::to_string(&event) {
        Ok(payload) => info!(
            target: SESSION_TARGET,
            event = EVENT_PUBLISH_DEGRADATION,
            session_id,
            fallback,
            outcome,
            payload = %payload
        ),
        Err(err) => warn!(
            target: SESSION_TARGET,
            event = EVENT_PUBLISH_DEGRADATION,
            %err,
            "failed to encode publish degradation event"
        ),
    }
}

pub fn record_session_draft_saved(session_id: &str, draft_id: &str, tags: &[String]) {
    let tag_refs: Vec<&str> = tags.iter().map(|tag| tag.as_str()).collect();
    let event = SessionDraftSaveEvent {
        session_id,
        draft_id,
        tags: tag_refs,
    };

    match serde_json::to_string(&event) {
        Ok(payload) => info!(
            target: SESSION_TARGET,
            event = EVENT_DRAFT_SAVE_SUCCESS,
            session_id,
            draft_id,
            tag_count = event.tags.len(),
            payload = %payload
        ),
        Err(err) => warn!(
            target: SESSION_TARGET,
            event = EVENT_DRAFT_SAVE_SUCCESS,
            %err,
            "failed to encode draft save event"
        ),
    }
}

pub fn record_session_draft_failed(session_id: &str, error: String) {
    let event = SessionDraftSaveFailureEvent {
        session_id,
        error: &error,
    };

    match serde_json::to_string(&event) {
        Ok(payload) => warn!(
            target: SESSION_TARGET,
            event = EVENT_DRAFT_SAVE_FAILURE,
            session_id,
            error = %error,
            payload = %payload
        ),
        Err(err) => warn!(
            target: SESSION_TARGET,
            event = EVENT_DRAFT_SAVE_FAILURE,
            %err,
            "failed to encode draft save failure"
        ),
    }
}

pub fn record_session_publish_undo(session_id: &str, undo_token: Option<&str>, origin: &str) {
    let event = SessionPublishUndoEvent {
        session_id,
        undo_token,
        origin,
    };

    match serde_json::to_string(&event) {
        Ok(payload) => info!(
            target: SESSION_TARGET,
            event = EVENT_PUBLISH_UNDO,
            session_id,
            origin,
            payload = %payload
        ),
        Err(err) => warn!(
            target: SESSION_TARGET,
            event = EVENT_PUBLISH_UNDO,
            %err,
            "failed to encode publish undo event"
        ),
    }
}

pub fn record_session_history_persisted(session_id: &str, attempts: u8, duration: Duration) {
    info!(
        target: SESSION_TARGET,
        event = EVENT_HISTORY_PERSISTED,
        session_id,
        attempts,
        duration_ms = duration_to_ms(duration),
        "session history persisted"
    );
}

pub fn record_session_history_persist_failure(session_id: &str, attempts: u8, error: &Error) {
    warn!(
        target: SESSION_TARGET,
        event = EVENT_HISTORY_PERSIST_FAILURE,
        session_id,
        attempts,
        error = %error,
        "session history persistence failed"
    );
}

pub fn record_session_history_accuracy(session_id: &str, flag: &str, remarks: Option<&str>) {
    info!(
        target: SESSION_TARGET,
        event = EVENT_HISTORY_ACCURACY,
        session_id,
        flag,
        remarks,
        "session history accuracy updated"
    );
}

pub fn record_session_history_action(session_id: &str, action: &str) {
    info!(
        target: SESSION_TARGET,
        event = EVENT_HISTORY_ACTION,
        session_id,
        action,
        "session history action recorded"
    );
}

pub fn record_session_history_cleanup(count: usize, duration: Duration) {
    info!(
        target: SESSION_TARGET,
        event = EVENT_HISTORY_CLEANUP,
        count,
        duration_ms = duration_to_ms(duration),
        "session history cleanup completed"
    );
}

fn duration_to_ms(duration: Duration) -> u64 {
    duration.as_millis().min(u64::MAX as u128) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duration_clamps_to_u64() {
        let duration = Duration::new(u64::MAX, 0);
        assert_eq!(duration_to_ms(duration), u64::MAX);
    }
}
