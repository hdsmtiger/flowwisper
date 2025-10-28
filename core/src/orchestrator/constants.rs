use std::time::Duration;

pub(crate) const SILENCE_RMS_THRESHOLD: f32 = 1e-4;
pub(crate) const SPEECH_RMS_THRESHOLD: f32 = 5e-4;
pub(crate) const CLOUD_RETRY_BACKOFF: Duration = Duration::from_millis(750);
