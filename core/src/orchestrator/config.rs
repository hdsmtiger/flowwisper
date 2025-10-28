use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineConfig {
    pub prefer_cloud: bool,
}

#[derive(Debug, Clone)]
pub struct RealtimeSessionConfig {
    pub sample_rate_hz: u32,
    pub min_frame_duration: Duration,
    pub max_frame_duration: Duration,
    pub first_update_deadline: Duration,
    pub buffer_capacity: usize,
    pub raw_emit_window: Duration,
    pub polish_emit_deadline: Duration,
    pub enable_polisher: bool,
}

impl Default for RealtimeSessionConfig {
    fn default() -> Self {
        Self {
            sample_rate_hz: 16_000,
            min_frame_duration: Duration::from_millis(100),
            max_frame_duration: Duration::from_millis(200),
            first_update_deadline: Duration::from_millis(400),
            buffer_capacity: 32,
            raw_emit_window: Duration::from_millis(200),
            polish_emit_deadline: Duration::from_millis(2_500),
            enable_polisher: true,
        }
    }
}
