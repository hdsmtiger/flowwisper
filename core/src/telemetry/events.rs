use serde::Serialize;
use std::time::Duration;
use tracing::{info, warn};

pub(crate) const TARGET: &str = "telemetry::dual_view";
pub(crate) const EVENT_LATENCY: &str = "dual_view_latency";
pub(crate) const EVENT_REVERT: &str = "dual_view_revert";

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
