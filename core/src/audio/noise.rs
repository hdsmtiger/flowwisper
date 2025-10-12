use std::collections::VecDeque;
use std::time::Duration;

use super::AudioCaptureStage;

/// Event emitted by the [`NoiseDetector`] to describe changes in the
/// environment noise conditions.
#[derive(Debug, Clone)]
pub enum NoiseEvent {
    /// Baseline ambient noise level has been established. Levels are expressed
    /// in dBFS (decibels relative to full scale).
    BaselineEstablished { level_db: f32 },
    /// A persistent noise spike has been detected that exceeds the baseline by
    /// at least the configured threshold.
    NoiseWarning(NoiseWarningPayload),
    /// Silence has persisted and a countdown toward auto-stop is underway.
    SilenceCountdown(SilenceCountdownPayload),
}

/// Structured payload describing a detected noise warning.
#[derive(Debug, Clone)]
pub struct NoiseWarningPayload {
    pub baseline_db: f32,
    pub threshold_db: f32,
    pub window_db: f32,
    pub persistence_ms: u32,
}

/// Enumerates the state transitions of a silence countdown timer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SilenceCountdownStatus {
    Started,
    Tick,
    Canceled,
    Completed,
}

/// Structured payload describing the progress of a silence countdown timer.
#[derive(Debug, Clone)]
pub struct SilenceCountdownPayload {
    pub total_ms: u32,
    pub remaining_ms: u32,
    pub status: SilenceCountdownStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BaselineState {
    Idle,
    Sampling,
    Locked,
}

/// Rolling noise and silence detector.
pub struct NoiseDetector {
    stage: AudioCaptureStage,
    baseline_state: BaselineState,
    baseline_db: Option<f32>,
    fallback_samples: usize,
    sampling_remaining: usize,
    sampling_energy: f64,
    sampling_samples: usize,
    analysis_window_samples: usize,
    analysis_pending: VecDeque<f32>,
    over_threshold_windows: usize,
    spike_active: bool,
    cooldown_windows: usize,
    silence_threshold_offset_db: f32,
    silence_countdown_ms: u32,
    silence_countdown_windows: usize,
    silence_windows: usize,
    silence_active: bool,
    silence_completed: bool,
}

impl NoiseDetector {
    pub fn new(sample_rate: u32) -> Self {
        let fallback_samples = duration_to_samples(Duration::from_millis(500), sample_rate);
        let analysis_window_samples = duration_to_samples(Duration::from_millis(100), sample_rate);
        let silence_countdown_ms = 5_000;
        let silence_countdown_windows = (duration_to_samples(
            Duration::from_millis(silence_countdown_ms as u64),
            sample_rate,
        ) / analysis_window_samples)
            .max(1);
        Self {
            stage: AudioCaptureStage::Idle,
            baseline_state: BaselineState::Idle,
            baseline_db: None,
            fallback_samples,
            sampling_remaining: fallback_samples,
            sampling_energy: 0.0,
            sampling_samples: 0,
            analysis_window_samples,
            analysis_pending: VecDeque::new(),
            over_threshold_windows: 0,
            spike_active: false,
            cooldown_windows: 0,
            silence_threshold_offset_db: 10.0,
            silence_countdown_ms,
            silence_countdown_windows,
            silence_windows: 0,
            silence_active: false,
            silence_completed: false,
        }
    }

    pub fn reset(&mut self) {
        self.stage = AudioCaptureStage::Idle;
        self.baseline_state = BaselineState::Idle;
        self.baseline_db = None;
        self.sampling_remaining = self.fallback_samples;
        self.sampling_energy = 0.0;
        self.sampling_samples = 0;
        self.analysis_pending.clear();
        self.over_threshold_windows = 0;
        self.spike_active = false;
        self.cooldown_windows = 0;
        self.silence_windows = 0;
        self.silence_active = false;
        self.silence_completed = false;
    }

    pub fn enter_preroll(&mut self, baseline_db: Option<f32>) -> Vec<NoiseEvent> {
        self.stage = AudioCaptureStage::PreRoll;
        self.sampling_energy = 0.0;
        self.sampling_samples = 0;
        self.sampling_remaining = self.fallback_samples;
        self.analysis_pending.clear();
        self.over_threshold_windows = 0;
        self.spike_active = false;
        self.cooldown_windows = 0;
        self.silence_windows = 0;
        self.silence_active = false;
        self.silence_completed = false;

        match baseline_db {
            Some(level) => {
                self.baseline_state = BaselineState::Locked;
                self.baseline_db = Some(level);
                vec![NoiseEvent::BaselineEstablished { level_db: level }]
            }
            None => {
                self.baseline_state = BaselineState::Sampling;
                self.baseline_db = None;
                Vec::new()
            }
        }
    }

    pub fn enter_recording(&mut self) {
        self.stage = AudioCaptureStage::Recording;
        self.analysis_pending.clear();
        self.over_threshold_windows = 0;
        self.spike_active = false;
        self.cooldown_windows = 0;
        self.silence_windows = 0;
        self.silence_active = false;
        self.silence_completed = false;
    }

    pub fn ingest(&mut self, samples: &[f32], stage: AudioCaptureStage) -> Vec<NoiseEvent> {
        if samples.is_empty() {
            return Vec::new();
        }

        if stage != self.stage {
            self.stage = stage;
        }

        match stage {
            AudioCaptureStage::PreRoll => self.ingest_preroll(samples),
            AudioCaptureStage::Recording => self.ingest_recording(samples),
            AudioCaptureStage::Idle => Vec::new(),
        }
    }

    fn ingest_preroll(&mut self, samples: &[f32]) -> Vec<NoiseEvent> {
        if self.baseline_state != BaselineState::Sampling {
            return Vec::new();
        }

        self.collect_baseline(samples)
    }

    fn ingest_recording(&mut self, samples: &[f32]) -> Vec<NoiseEvent> {
        let mut events = Vec::new();

        if self.baseline_state == BaselineState::Sampling {
            events.extend(self.collect_baseline(samples));
        }

        if self.baseline_state != BaselineState::Locked {
            return events;
        }

        self.analysis_pending.extend(samples.iter().copied());

        while self.analysis_pending.len() >= self.analysis_window_samples {
            let mut energy = 0.0_f64;
            for _ in 0..self.analysis_window_samples {
                if let Some(sample) = self.analysis_pending.pop_front() {
                    energy += f64::from(sample) * f64::from(sample);
                }
            }

            let rms = if self.analysis_window_samples > 0 {
                (energy / self.analysis_window_samples as f64).sqrt() as f32
            } else {
                0.0
            };

            let window_db = amplitude_to_db(rms);
            let baseline_db = self.baseline_db.expect("baseline locked implies value");
            let threshold = baseline_db + 15.0;

            if self.cooldown_windows > 0 {
                self.cooldown_windows -= 1;
            }

            if window_db >= threshold {
                self.over_threshold_windows += 1;
            } else {
                self.over_threshold_windows = 0;
                self.spike_active = false;
            }

            if self.over_threshold_windows >= 3 && !self.spike_active && self.cooldown_windows == 0
            {
                self.spike_active = true;
                self.cooldown_windows = 20;
                events.push(NoiseEvent::NoiseWarning(NoiseWarningPayload {
                    baseline_db,
                    threshold_db: threshold,
                    window_db,
                    persistence_ms: (self.over_threshold_windows as u32) * 100,
                }));
            }

            self.evaluate_silence(window_db, baseline_db, &mut events);
        }

        events
    }

    fn evaluate_silence(&mut self, window_db: f32, baseline_db: f32, events: &mut Vec<NoiseEvent>) {
        let threshold = baseline_db - self.silence_threshold_offset_db;

        if window_db <= threshold {
            if self.silence_completed {
                return;
            }

            self.silence_windows += 1;
            let countdown_windows = self.silence_countdown_windows.max(1);
            let elapsed_windows = self.silence_windows.min(countdown_windows);
            let elapsed_ms = (elapsed_windows as u32) * 100;
            let remaining_ms = self
                .silence_countdown_ms
                .saturating_sub(elapsed_ms)
                .min(self.silence_countdown_ms);

            let status = if self.silence_windows == 1 {
                SilenceCountdownStatus::Started
            } else if remaining_ms == 0 {
                SilenceCountdownStatus::Completed
            } else {
                SilenceCountdownStatus::Tick
            };

            self.silence_active = true;

            if status == SilenceCountdownStatus::Completed {
                self.silence_completed = true;
                self.silence_active = false;
                self.silence_windows = countdown_windows;
            }

            events.push(NoiseEvent::SilenceCountdown(SilenceCountdownPayload {
                total_ms: self.silence_countdown_ms,
                remaining_ms,
                status,
            }));
        } else if self.silence_windows > 0 || self.silence_active {
            if !self.silence_completed {
                events.push(NoiseEvent::SilenceCountdown(SilenceCountdownPayload {
                    total_ms: self.silence_countdown_ms,
                    remaining_ms: self.silence_countdown_ms,
                    status: SilenceCountdownStatus::Canceled,
                }));
            }

            self.silence_windows = 0;
            self.silence_active = false;
            self.silence_completed = false;
        }
    }

    fn collect_baseline(&mut self, samples: &[f32]) -> Vec<NoiseEvent> {
        if self.sampling_remaining == 0 {
            return Vec::new();
        }

        let mut offset = 0;
        while offset < samples.len() && self.sampling_remaining > 0 {
            let take = (samples.len() - offset).min(self.sampling_remaining);
            let chunk = &samples[offset..offset + take];
            let energy: f64 = chunk
                .iter()
                .map(|sample| f64::from(*sample) * f64::from(*sample))
                .sum();
            self.sampling_energy += energy;
            self.sampling_samples += take;
            self.sampling_remaining -= take;
            offset += take;
        }

        if self.sampling_remaining == 0 {
            let level_db = if self.sampling_samples > 0 {
                let rms = (self.sampling_energy / self.sampling_samples as f64).sqrt() as f32;
                amplitude_to_db(rms)
            } else {
                -120.0
            };

            self.baseline_state = BaselineState::Locked;
            self.baseline_db = Some(level_db);
            vec![NoiseEvent::BaselineEstablished { level_db }]
        } else {
            Vec::new()
        }
    }

    pub fn baseline_db(&self) -> Option<f32> {
        self.baseline_db
    }
}

fn duration_to_samples(duration: Duration, sample_rate: u32) -> usize {
    ((duration.as_secs_f64() * sample_rate as f64).round() as usize).max(1)
}

fn amplitude_to_db(amplitude: f32) -> f32 {
    let clamped = amplitude.abs().max(1e-9);
    20.0 * clamped.log10()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uses_provided_baseline_during_preroll() {
        let mut detector = NoiseDetector::new(16_000);
        let events = detector.enter_preroll(Some(-32.0));

        assert_eq!(events.len(), 1);
        match &events[0] {
            NoiseEvent::BaselineEstablished { level_db } => {
                assert!((level_db + 32.0).abs() < f32::EPSILON);
            }
            NoiseEvent::NoiseWarning(_) => panic!("unexpected noise warning"),
            NoiseEvent::SilenceCountdown(_) => panic!("unexpected silence countdown"),
        }
        assert_eq!(detector.baseline_db(), Some(-32.0));
    }

    #[test]
    fn samples_baseline_when_not_provided() {
        let mut detector = NoiseDetector::new(16_000);
        let events = detector.enter_preroll(None);
        assert!(events.is_empty());

        let samples = vec![0.1_f32; 8_000];
        let events = detector.ingest(&samples, AudioCaptureStage::PreRoll);
        assert_eq!(events.len(), 1, "baseline event not emitted");

        let level = match &events[0] {
            NoiseEvent::BaselineEstablished { level_db } => *level_db,
            NoiseEvent::NoiseWarning(_) => panic!("unexpected noise warning"),
            NoiseEvent::SilenceCountdown(_) => panic!("unexpected silence countdown"),
        };

        assert!(
            (level + 20.0).abs() < 0.5,
            "unexpected baseline level: {level}"
        );
        assert!(detector.baseline_db().is_some());

        let subsequent = detector.ingest(&samples, AudioCaptureStage::PreRoll);
        assert!(subsequent.is_empty(), "duplicate baseline events emitted");
    }

    #[test]
    fn emits_noise_warning_after_persistent_spike() {
        let mut detector = NoiseDetector::new(16_000);
        detector.enter_preroll(None);

        let baseline_samples = vec![0.01_f32; 8_000];
        let events = detector.ingest(&baseline_samples, AudioCaptureStage::PreRoll);
        assert_eq!(events.len(), 1, "baseline not established during preroll");

        detector.enter_recording();

        let quiet_window = vec![0.01_f32; 1_600];
        let events = detector.ingest(&quiet_window, AudioCaptureStage::Recording);
        assert!(events.is_empty(), "quiet window should not emit warning");

        let loud_window = vec![0.5_f32; 1_600];

        let events = detector.ingest(&loud_window, AudioCaptureStage::Recording);
        assert!(
            events.is_empty(),
            "first loud window should not yet emit warning"
        );

        let events = detector.ingest(&loud_window, AudioCaptureStage::Recording);
        assert!(
            events.is_empty(),
            "second loud window should accumulate persistence"
        );

        let events = detector.ingest(&loud_window, AudioCaptureStage::Recording);
        assert_eq!(events.len(), 1, "third loud window should emit warning");

        match &events[0] {
            NoiseEvent::NoiseWarning(payload) => {
                assert!((payload.persistence_ms as usize) >= 300);
                assert!(payload.window_db - payload.baseline_db >= 15.0);
                assert!((payload.threshold_db - (payload.baseline_db + 15.0)).abs() < 1e-3);
            }
            NoiseEvent::BaselineEstablished { .. } => {
                panic!("expected noise warning, received baseline event");
            }
            NoiseEvent::SilenceCountdown(_) => {
                panic!("unexpected silence countdown event during noise spike");
            }
        }

        let events = detector.ingest(&quiet_window, AudioCaptureStage::Recording);
        assert!(
            events.is_empty(),
            "returning to quiet should reset detection"
        );

        for _ in 0..19 {
            let events = detector.ingest(&quiet_window, AudioCaptureStage::Recording);
            assert!(
                events.is_empty(),
                "cooldown windows should suppress events while counting down"
            );
        }

        let events = detector.ingest(&loud_window, AudioCaptureStage::Recording);
        assert!(
            events.is_empty(),
            "cooldown should require persistence to restart"
        );

        let events = detector.ingest(&loud_window, AudioCaptureStage::Recording);
        assert!(
            events.is_empty(),
            "second loud window starts persistence after cooldown"
        );

        let events = detector.ingest(&loud_window, AudioCaptureStage::Recording);
        assert_eq!(
            events.len(),
            1,
            "cooldown elapsed should allow second warning"
        );

        match &events[0] {
            NoiseEvent::NoiseWarning(payload) => {
                assert!((payload.persistence_ms as usize) >= 300);
                assert!(payload.window_db - payload.baseline_db >= 15.0);
                assert!((payload.threshold_db - (payload.baseline_db + 15.0)).abs() < 1e-3);
            }
            NoiseEvent::BaselineEstablished { .. } => {
                panic!("expected noise warning, received baseline event");
            }
            NoiseEvent::SilenceCountdown(_) => {
                panic!("unexpected silence countdown event during noise spike");
            }
        }
    }

    #[test]
    fn silence_countdown_completes_after_5_seconds() {
        let mut detector = NoiseDetector::new(16_000);
        detector.enter_preroll(None);

        let baseline_samples = vec![0.05_f32; 8_000];
        let events = detector.ingest(&baseline_samples, AudioCaptureStage::PreRoll);
        assert_eq!(events.len(), 1);

        detector.enter_recording();

        let quiet_window = vec![0.005_f32; 1_600];
        for step in 1..=50 {
            let events = detector.ingest(&quiet_window, AudioCaptureStage::Recording);
            assert_eq!(events.len(), 1, "expected countdown event on step {step}");

            match &events[0] {
                NoiseEvent::SilenceCountdown(payload) => {
                    assert_eq!(payload.total_ms, 5_000);
                    match payload.status {
                        SilenceCountdownStatus::Started => {
                            assert_eq!(step, 1);
                            assert_eq!(payload.remaining_ms, 4_900);
                        }
                        SilenceCountdownStatus::Tick => {
                            assert!(step > 1 && step < 50);
                            assert!(payload.remaining_ms < 5_000);
                            assert!(payload.remaining_ms > 0);
                        }
                        SilenceCountdownStatus::Completed => {
                            assert_eq!(step, 50);
                            assert_eq!(payload.remaining_ms, 0);
                        }
                        SilenceCountdownStatus::Canceled => {
                            panic!("did not expect cancellation during continuous silence");
                        }
                    }
                }
                _ => panic!("unexpected event emitted during silence countdown"),
            }
        }

        let events = detector.ingest(&quiet_window, AudioCaptureStage::Recording);
        assert!(
            events.is_empty(),
            "countdown completion should suppress further events"
        );
    }

    #[test]
    fn silence_countdown_cancels_on_speech_return() {
        let mut detector = NoiseDetector::new(16_000);
        detector.enter_preroll(None);

        let baseline_samples = vec![0.05_f32; 8_000];
        let events = detector.ingest(&baseline_samples, AudioCaptureStage::PreRoll);
        assert_eq!(events.len(), 1);

        detector.enter_recording();

        let quiet_window = vec![0.005_f32; 1_600];
        for _ in 0..5 {
            let events = detector.ingest(&quiet_window, AudioCaptureStage::Recording);
            assert!(!events.is_empty());
        }

        let loud_window = vec![0.05_f32; 1_600];
        let events = detector.ingest(&loud_window, AudioCaptureStage::Recording);
        assert_eq!(events.len(), 1, "speech return should emit cancellation");

        match &events[0] {
            NoiseEvent::SilenceCountdown(payload) => {
                assert_eq!(payload.status, SilenceCountdownStatus::Canceled);
                assert_eq!(payload.remaining_ms, 5_000);
            }
            _ => panic!("expected silence countdown cancellation event"),
        }

        let events = detector.ingest(&quiet_window, AudioCaptureStage::Recording);
        assert_eq!(events.len(), 1, "new silence should restart countdown");
        match &events[0] {
            NoiseEvent::SilenceCountdown(payload) => {
                assert_eq!(payload.status, SilenceCountdownStatus::Started);
            }
            _ => panic!("expected countdown restart"),
        }
    }
}
