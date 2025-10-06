//! 引擎编排服务脚手架。

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::sync::{
    atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    Arc,
};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, Mutex, Notify};
use tokio::task::JoinHandle;
use tokio::time::{sleep, sleep_until, timeout, Instant as TokioInstant};
use tracing::{error, info, warn};

const SILENCE_RMS_THRESHOLD: f32 = 1e-4;
const SPEECH_RMS_THRESHOLD: f32 = 5e-4;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineConfig {
    pub prefer_cloud: bool,
}

#[async_trait]
pub trait SpeechEngine: Send + Sync {
    async fn transcribe(&self, frame: &[f32]) -> Result<String>;
}

pub struct EngineOrchestrator {
    config: EngineConfig,
    local_engine: Arc<dyn SpeechEngine>,
    cloud_engine: Option<Arc<dyn SpeechEngine>>,
}

impl EngineOrchestrator {
    pub fn new(config: EngineConfig) -> Result<Self> {
        let local_engine = Self::build_local_engine()?;
        Ok(Self::with_engines(config, local_engine, None))
    }

    pub fn with_engine(config: EngineConfig, local_engine: Arc<dyn SpeechEngine>) -> Self {
        Self::with_engines(config, local_engine, None)
    }

    pub fn with_engines(
        config: EngineConfig,
        local_engine: Arc<dyn SpeechEngine>,
        cloud_engine: Option<Arc<dyn SpeechEngine>>,
    ) -> Self {
        Self {
            config,
            local_engine,
            cloud_engine,
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
        let (tx, rx) = mpsc::channel(config.buffer_capacity);
        let (frame_tx, frame_rx) = mpsc::channel(config.buffer_capacity);
        let first_update_flag = Arc::new(AtomicBool::new(false));
        let first_local_update_flag = Arc::new(AtomicBool::new(false));
        let local_progress = Arc::new(LocalProgress::new());
        let local_update_notify = Arc::new(Notify::new());
        let local_serial = Arc::new(Mutex::new(()));
        let started_at = Instant::now();
        let monitor_progress = local_progress.clone();
        let monitor_tx = tx.clone();
        let deadline = config.first_update_deadline;
        let cadence = if config.max_frame_duration.is_zero() {
            config.min_frame_duration
        } else {
            config.max_frame_duration.max(config.min_frame_duration)
        };

        let monitor: JoinHandle<()> = tokio::spawn(async move {
            let poll_interval = Duration::from_millis(25);
            let mut first_window = true;
            let mut last_seen_frame = 0_u64;
            let mut violation_active = false;

            loop {
                let wait = if first_window { poll_interval } else { cadence };
                sleep(wait).await;

                if monitor_tx.is_closed() {
                    break;
                }

                let current_frame = monitor_progress.last_frame();
                let degraded = monitor_progress.is_degraded();

                if first_window {
                    if current_frame > 0 {
                        last_seen_frame = current_frame;
                        violation_active = false;
                        first_window = false;
                        continue;
                    }

                    let speech_started_ms = monitor_progress.speech_started_ms();
                    if speech_started_ms == 0 {
                        continue;
                    }

                    let elapsed_since_speech = started_at
                        .elapsed()
                        .saturating_sub(Duration::from_millis(speech_started_ms));

                    if elapsed_since_speech >= deadline && !violation_active {
                        warn!(
                            target: "engine_orchestrator",
                            elapsed = ?elapsed_since_speech,
                            "first transcription update missed {:?} deadline",
                            deadline
                        );

                        monitor_progress.mark_degraded(started_at);

                        let notice = TranscriptionUpdate {
                            payload: UpdatePayload::Notice(SessionNotice {
                                level: NoticeLevel::Warn,
                                message: "本地解码延迟异常，已保留回退提示".to_string(),
                            }),
                            latency: elapsed_since_speech,
                            frame_index: 0,
                            is_first: false,
                        };

                        if let Err(err) = monitor_tx.send(notice).await {
                            warn!(
                                target: "engine_orchestrator",
                                %err,
                                "failed to deliver deadline fallback notice"
                            );
                        }
                        violation_active = true;
                    } else if elapsed_since_speech < deadline {
                        violation_active = false;
                    }

                    continue;
                }

                if current_frame > last_seen_frame {
                    last_seen_frame = current_frame;
                    violation_active = false;
                    continue;
                }

                if degraded {
                    violation_active = false;
                    continue;
                }

                let elapsed_ms = duration_to_ms(started_at.elapsed());
                let last_update_ms = monitor_progress.last_update_ms();
                let since_ms = elapsed_ms.saturating_sub(last_update_ms);
                let cadence_ms = duration_to_ms(cadence);

                if !monitor_progress.is_speech_active() {
                    violation_active = false;
                    continue;
                }

                if since_ms >= cadence_ms && !violation_active {
                    warn!(
                        target: "engine_orchestrator",
                        elapsed_ms,
                        last_update_ms,
                        "local transcription cadence degraded"
                    );

                    monitor_progress.mark_degraded(started_at);

                    let notice = TranscriptionUpdate {
                        payload: UpdatePayload::Notice(SessionNotice {
                            level: NoticeLevel::Warn,
                            message: "本地解码增量延迟，已保留回退提示".to_string(),
                        }),
                        latency: Duration::from_millis(since_ms),
                        frame_index: last_seen_frame as usize,
                        is_first: false,
                    };

                    if let Err(err) = monitor_tx.send(notice).await {
                        warn!(
                            target: "engine_orchestrator",
                            %err,
                            "failed to deliver rolling cadence notice"
                        );
                    }
                    violation_active = true;
                } else if since_ms < cadence_ms {
                    violation_active = false;
                }
            }
        });

        let worker = RealtimeWorker::new(
            config.clone(),
            frame_rx,
            tx.clone(),
            Arc::clone(&self.local_engine),
            self.cloud_engine.clone(),
            first_update_flag.clone(),
            first_local_update_flag.clone(),
            local_progress.clone(),
            local_update_notify.clone(),
            Arc::clone(&local_serial),
            started_at,
            self.config.prefer_cloud,
        );

        let handle = RealtimeSessionHandle {
            config,
            frame_tx,
            updates_tx: tx,
            first_update_flag,
            first_local_update_flag,
            local_progress,
            local_update_notify,
            started_at,
            monitor: Some(monitor),
            worker: Some(worker.spawn()),
        };

        (handle, rx)
    }

    fn build_local_engine() -> Result<Arc<dyn SpeechEngine>> {
        #[cfg(feature = "local-asr")]
        {
            return match WhisperLocalEngine::from_env() {
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

#[derive(Debug, Clone)]
pub struct RealtimeSessionConfig {
    pub sample_rate_hz: u32,
    pub min_frame_duration: Duration,
    pub max_frame_duration: Duration,
    pub first_update_deadline: Duration,
    pub buffer_capacity: usize,
}

impl Default for RealtimeSessionConfig {
    fn default() -> Self {
        Self {
            sample_rate_hz: 16_000,
            min_frame_duration: Duration::from_millis(100),
            max_frame_duration: Duration::from_millis(200),
            first_update_deadline: Duration::from_millis(400),
            buffer_capacity: 32,
        }
    }
}

#[derive(Debug, Clone)]
pub enum UpdatePayload {
    Transcript(TranscriptPayload),
    Notice(SessionNotice),
}

#[derive(Debug, Clone)]
pub struct TranscriptPayload {
    pub text: String,
    pub source: TranscriptSource,
    pub is_primary: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranscriptSource {
    Local,
    Cloud,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeLevel {
    Info,
    Warn,
    Error,
}

#[derive(Debug, Clone)]
pub struct SessionNotice {
    pub level: NoticeLevel,
    pub message: String,
}

#[derive(Debug, Clone)]
pub struct TranscriptionUpdate {
    pub payload: UpdatePayload,
    pub latency: Duration,
    pub frame_index: usize,
    pub is_first: bool,
}

#[derive(Default)]
struct LocalProgress {
    last_frame: AtomicU64,
    degraded: AtomicBool,
    last_update_ms: AtomicU64,
    speech_started_ms: AtomicU64,
    speech_active: AtomicBool,
}

impl LocalProgress {
    fn new() -> Self {
        Self::default()
    }

    fn record_success(&self, frame_index: usize, started_at: Instant) {
        let new_index = frame_index as u64;
        let mut current = self.last_frame.load(Ordering::SeqCst);

        loop {
            if current >= new_index {
                self.mark_speech_detected(started_at);
                return;
            }

            match self.last_frame.compare_exchange(
                current,
                new_index,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => {
                    self.degraded.store(false, Ordering::SeqCst);
                    self.last_update_ms
                        .store(duration_to_ms(started_at.elapsed()), Ordering::SeqCst);
                    self.mark_speech_detected(started_at);
                    return;
                }
                Err(actual) => current = actual,
            }
        }
    }

    fn mark_degraded(&self, started_at: Instant) {
        self.degraded.store(true, Ordering::SeqCst);
        self.last_update_ms
            .store(duration_to_ms(started_at.elapsed()), Ordering::SeqCst);
    }

    fn record_frame_energy(&self, started_at: Instant, rms: f32) {
        if rms >= SPEECH_RMS_THRESHOLD {
            self.speech_active.store(true, Ordering::SeqCst);
            self.mark_speech_detected(started_at);
        } else if rms <= SILENCE_RMS_THRESHOLD {
            self.speech_active.store(false, Ordering::SeqCst);
        }
    }

    fn last_frame(&self) -> u64 {
        self.last_frame.load(Ordering::SeqCst)
    }

    fn is_degraded(&self) -> bool {
        self.degraded.load(Ordering::SeqCst)
    }

    fn last_update_ms(&self) -> u64 {
        self.last_update_ms.load(Ordering::SeqCst)
    }

    fn mark_speech_detected(&self, started_at: Instant) {
        let detected_ms = duration_to_ms(started_at.elapsed()).max(1);
        let _ = self.speech_started_ms.compare_exchange(
            0,
            detected_ms,
            Ordering::SeqCst,
            Ordering::SeqCst,
        );
    }

    fn speech_started_ms(&self) -> u64 {
        self.speech_started_ms.load(Ordering::SeqCst)
    }

    fn has_speech_started(&self) -> bool {
        self.speech_started_ms() != 0
    }

    fn is_speech_active(&self) -> bool {
        self.speech_active.load(Ordering::SeqCst)
    }
}

pub struct RealtimeSessionHandle {
    config: RealtimeSessionConfig,
    frame_tx: mpsc::Sender<Arc<[f32]>>,
    updates_tx: mpsc::Sender<TranscriptionUpdate>,
    first_update_flag: Arc<AtomicBool>,
    first_local_update_flag: Arc<AtomicBool>,
    local_progress: Arc<LocalProgress>,
    local_update_notify: Arc<Notify>,
    started_at: Instant,
    monitor: Option<JoinHandle<()>>,
    worker: Option<JoinHandle<()>>,
}

impl RealtimeSessionHandle {
    pub async fn push_frame(
        &self,
        frame: Vec<f32>,
    ) -> Result<(), mpsc::error::SendError<Arc<[f32]>>> {
        if frame.is_empty() {
            warn!(target: "engine_orchestrator", "received empty audio frame");
            return Ok(());
        }

        let frame_duration =
            Duration::from_secs_f64(frame.len() as f64 / self.config.sample_rate_hz as f64);

        if frame_duration < self.config.min_frame_duration
            || frame_duration > self.config.max_frame_duration
        {
            warn!(
                target: "engine_orchestrator",
                ?frame_duration,
                min = ?self.config.min_frame_duration,
                max = ?self.config.max_frame_duration,
                "audio frame duration out of expected bounds"
            );
        }

        let shared: Arc<[f32]> = frame.into();
        match self.frame_tx.send(shared).await {
            Ok(()) => Ok(()),
            Err(err) => {
                warn!(
                    target: "engine_orchestrator",
                    %err,
                    "failed to enqueue audio frame"
                );
                Err(err)
            }
        }
    }

    pub fn frame_sender(&self) -> mpsc::Sender<Arc<[f32]>> {
        self.frame_tx.clone()
    }
}

impl Drop for RealtimeSessionHandle {
    fn drop(&mut self) {
        if let Some(handle) = self.monitor.take() {
            handle.abort();
        }
        if let Some(worker) = self.worker.take() {
            worker.abort();
        }
    }
}

const CLOUD_RETRY_BACKOFF: Duration = Duration::from_millis(750);

struct RealtimeWorker {
    config: RealtimeSessionConfig,
    frame_rx: mpsc::Receiver<Arc<[f32]>>,
    updates_tx: mpsc::Sender<TranscriptionUpdate>,
    local_engine: Arc<dyn SpeechEngine>,
    cloud_engine: Option<Arc<dyn SpeechEngine>>,
    first_update_flag: Arc<AtomicBool>,
    first_local_update_flag: Arc<AtomicBool>,
    local_progress: Arc<LocalProgress>,
    local_update_notify: Arc<Notify>,
    local_serial: Arc<Mutex<()>>,
    started_at: Instant,
    prefer_cloud: bool,
}

struct CloudCircuit {
    enabled: AtomicBool,
    next_retry_ms: AtomicU64,
}

impl CloudCircuit {
    fn new() -> Self {
        Self {
            enabled: AtomicBool::new(true),
            next_retry_ms: AtomicU64::new(0),
        }
    }

    fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::SeqCst)
    }

    fn allow_attempt(&self, started_at: Instant, now: Instant) -> bool {
        if self.is_enabled() {
            return true;
        }

        let elapsed_ms = duration_to_ms(now.saturating_duration_since(started_at));
        let next_ms = self.next_retry_ms.load(Ordering::SeqCst);
        if elapsed_ms >= next_ms {
            self.enabled.store(true, Ordering::SeqCst);
            true
        } else {
            false
        }
    }

    fn mark_success(&self) {
        self.next_retry_ms.store(0, Ordering::SeqCst);
        self.enabled.store(true, Ordering::SeqCst);
    }

    fn trip(&self, started_at: Instant, now: Instant, backoff: Duration) -> bool {
        let elapsed_ms = duration_to_ms(now.saturating_duration_since(started_at));
        let backoff_ms = duration_to_ms(backoff);
        let next_retry = elapsed_ms.saturating_add(backoff_ms);
        self.next_retry_ms.store(next_retry, Ordering::SeqCst);
        let was_enabled = self.enabled.swap(false, Ordering::SeqCst);
        was_enabled
    }
}

fn duration_to_ms(duration: Duration) -> u64 {
    let millis = duration.as_millis();
    if millis > u128::from(u64::MAX) {
        u64::MAX
    } else {
        millis as u64
    }
}

fn frame_rms(frame: &[f32]) -> f32 {
    if frame.is_empty() {
        return 0.0;
    }

    let energy: f32 = frame.iter().map(|sample| sample * sample).sum();
    (energy / frame.len() as f32).sqrt()
}

fn suffix_prefix_overlap(existing: &str, new_text: &str) -> usize {
    let max = existing.len().min(new_text.len());
    for overlap in (1..=max).rev() {
        if existing.ends_with(&new_text[..overlap]) {
            return overlap;
        }
    }
    0
}

impl RealtimeWorker {
    fn new(
        config: RealtimeSessionConfig,
        frame_rx: mpsc::Receiver<Arc<[f32]>>,
        updates_tx: mpsc::Sender<TranscriptionUpdate>,
        local_engine: Arc<dyn SpeechEngine>,
        cloud_engine: Option<Arc<dyn SpeechEngine>>,
        first_update_flag: Arc<AtomicBool>,
        first_local_update_flag: Arc<AtomicBool>,
        local_progress: Arc<LocalProgress>,
        local_update_notify: Arc<Notify>,
        local_serial: Arc<Mutex<()>>,
        started_at: Instant,
        prefer_cloud: bool,
    ) -> Self {
        Self {
            config,
            frame_rx,
            updates_tx,
            local_engine,
            cloud_engine,
            first_update_flag,
            first_local_update_flag,
            local_progress,
            local_update_notify,
            local_serial,
            started_at,
            prefer_cloud,
        }
    }

    fn spawn(self) -> JoinHandle<()> {
        tokio::spawn(async move {
            self.run().await;
        })
    }

    async fn run(mut self) {
        let mut frame_index: usize = 0;
        let cloud_circuit = self
            .cloud_engine
            .as_ref()
            .map(|_| Arc::new(CloudCircuit::new()));
        let mut next_schedule = TokioInstant::now();

        while let Some(frame) = self.frame_rx.recv().await {
            frame_index += 1;

            let frame_duration =
                Duration::from_secs_f64(frame.len() as f64 / self.config.sample_rate_hz as f64);

            let pacing_step = frame_duration.max(self.config.min_frame_duration);
            let now = TokioInstant::now();
            if now < next_schedule {
                sleep_until(next_schedule).await;
            }
            next_schedule = TokioInstant::now() + pacing_step;

            let frame_started = Instant::now();
            let rms = frame_rms(frame.as_ref());
            self.local_progress
                .record_frame_energy(self.started_at, rms);

            self.spawn_local_task(
                frame.clone(),
                frame_index,
                frame_started,
                cloud_circuit.as_ref().map(Arc::clone),
            );

            if let (Some(cloud_engine), Some(circuit)) =
                (self.cloud_engine.clone(), cloud_circuit.as_ref())
            {
                let now = Instant::now();
                if circuit.allow_attempt(self.started_at, now) {
                    self.spawn_cloud_task(
                        frame.clone(),
                        frame_index,
                        frame_started,
                        cloud_engine,
                        Arc::clone(circuit),
                    );
                }
            }
        }
    }

    fn spawn_local_task(
        &self,
        frame: Arc<[f32]>,
        frame_index: usize,
        frame_started: Instant,
        _cloud_state: Option<Arc<CloudCircuit>>,
    ) {
        let engine = Arc::clone(&self.local_engine);
        let tx = self.updates_tx.clone();
        let first_flag = self.first_update_flag.clone();
        let first_local_flag = self.first_local_update_flag.clone();
        let local_progress = self.local_progress.clone();
        let local_notify = self.local_update_notify.clone();
        let local_serial = self.local_serial.clone();
        let started_at = self.started_at;

        tokio::spawn(async move {
            let _guard = local_serial.lock().await;
            match engine.transcribe(frame.as_ref()).await {
                Ok(text) if !text.is_empty() => {
                    let claimed_first = first_flag
                        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                        .is_ok();
                    let was_first_local = !first_local_flag.load(Ordering::SeqCst);
                    let is_primary = !local_progress.is_degraded();

                    let update = TranscriptionUpdate {
                        payload: UpdatePayload::Transcript(TranscriptPayload {
                            text,
                            source: TranscriptSource::Local,
                            is_primary,
                        }),
                        latency: frame_started.elapsed(),
                        frame_index,
                        is_first: claimed_first,
                    };

                    match tx.send(update).await {
                        Ok(_) => {
                            if was_first_local {
                                let _ = first_local_flag.compare_exchange(
                                    false,
                                    true,
                                    Ordering::SeqCst,
                                    Ordering::SeqCst,
                                );
                            }
                            local_progress.record_success(frame_index, started_at);
                            local_notify.notify_waiters();
                        }
                        Err(err) => {
                            if claimed_first {
                                first_flag.store(false, Ordering::SeqCst);
                            }
                            if was_first_local {
                                first_local_flag.store(false, Ordering::SeqCst);
                            }
                            local_progress.mark_degraded(started_at);
                            local_notify.notify_waiters();
                            warn!(
                                target: "engine_orchestrator",
                                %err,
                                "failed to deliver local transcription update"
                            );

                            let notice_message = if frame_index == 1 {
                                "本地解码延迟异常，已保留回退提示"
                            } else {
                                "本地解码增量延迟，已保留回退提示"
                            };

                            let notice = TranscriptionUpdate {
                                payload: UpdatePayload::Notice(SessionNotice {
                                    level: NoticeLevel::Warn,
                                    message: notice_message.to_string(),
                                }),
                                latency: frame_started.elapsed(),
                                frame_index,
                                is_first: false,
                            };

                            if let Err(err) = tx.send(notice).await {
                                warn!(
                                    target: "engine_orchestrator",
                                    %err,
                                    "failed to deliver local backpressure notice"
                                );
                            }
                        }
                    }
                }
                Ok(_) => {}
                Err(err) => {
                    error!(
                        target: "engine_orchestrator",
                        %err,
                        frame_index,
                        "local speech engine failed to transcribe frame"
                    );

                    local_progress.mark_degraded(started_at);
                    local_notify.notify_waiters();

                    let notice = TranscriptionUpdate {
                        payload: UpdatePayload::Notice(SessionNotice {
                            level: NoticeLevel::Error,
                            message: "本地识别异常，已切换云端回退".to_string(),
                        }),
                        latency: frame_started.elapsed(),
                        frame_index,
                        is_first: false,
                    };

                    if let Err(err) = tx.send(notice).await {
                        warn!(
                            target: "engine_orchestrator",
                            %err,
                            "failed to deliver local fallback notice"
                        );
                    }
                }
            }
        });
    }

    fn spawn_cloud_task(
        &self,
        frame: Arc<[f32]>,
        frame_index: usize,
        frame_started: Instant,
        engine: Arc<dyn SpeechEngine>,
        cloud_state: Arc<CloudCircuit>,
    ) {
        let tx = self.updates_tx.clone();
        let first_flag = self.first_update_flag.clone();
        let first_local_flag = self.first_local_update_flag.clone();
        let local_progress = self.local_progress.clone();
        let local_notify = self.local_update_notify.clone();
        let started_at = self.started_at;
        let prefer_cloud = self.prefer_cloud;
        let local_deadline = self.config.first_update_deadline;
        let cadence = if self.config.max_frame_duration.is_zero() {
            self.config.min_frame_duration
        } else {
            self.config
                .max_frame_duration
                .max(self.config.min_frame_duration)
        };

        tokio::spawn(async move {
            let mut timed_out = false;

            if local_progress.last_frame() < frame_index as u64 && !local_progress.is_degraded() {
                let gate_deadline = if frame_index == 1 {
                    local_deadline
                } else {
                    cadence
                };

                loop {
                    if local_progress.last_frame() >= frame_index as u64
                        || local_progress.is_degraded()
                    {
                        break;
                    }

                    let elapsed = frame_started.elapsed();
                    if elapsed >= gate_deadline {
                        timed_out = true;
                        break;
                    }

                    let remaining = gate_deadline - elapsed;
                    let _ = timeout(remaining, local_notify.notified()).await;
                }
            }

            if timed_out
                && (!local_progress.has_speech_started() || !local_progress.is_speech_active())
            {
                timed_out = false;
            }

            if timed_out && !local_progress.is_degraded() {
                local_progress.mark_degraded(started_at);
                local_notify.notify_waiters();

                let notice_message = if frame_index == 1 {
                    "本地解码延迟异常，已保留回退提示"
                } else {
                    "本地解码增量延迟，已保留回退提示"
                };

                let notice = TranscriptionUpdate {
                    payload: UpdatePayload::Notice(SessionNotice {
                        level: NoticeLevel::Warn,
                        message: notice_message.to_string(),
                    }),
                    latency: frame_started.elapsed(),
                    frame_index,
                    is_first: false,
                };

                if let Err(err) = tx.send(notice).await {
                    warn!(
                        target: "engine_orchestrator",
                        %err,
                        "failed to deliver cadence fallback notice"
                    );
                }
            }

            match engine.transcribe(frame.as_ref()).await {
                Ok(text) if !text.is_empty() => {
                    cloud_state.mark_success();
                    let is_first = if prefer_cloud {
                        if first_local_flag.load(Ordering::SeqCst) {
                            !first_flag.swap(true, Ordering::SeqCst)
                        } else {
                            let _ = first_flag.swap(true, Ordering::SeqCst);
                            false
                        }
                    } else {
                        first_flag.store(true, Ordering::SeqCst);
                        false
                    };
                    let update = TranscriptionUpdate {
                        payload: UpdatePayload::Transcript(TranscriptPayload {
                            text,
                            source: TranscriptSource::Cloud,
                            is_primary: local_progress.is_degraded(),
                        }),
                        latency: frame_started.elapsed(),
                        frame_index,
                        is_first,
                    };

                    if let Err(err) = tx.send(update).await {
                        warn!(
                            target: "engine_orchestrator",
                            %err,
                            "failed to deliver cloud transcription update"
                        );
                    }
                }
                Ok(_) => {}
                Err(err) => {
                    warn!(
                        target: "engine_orchestrator",
                        %err,
                        frame_index,
                        "cloud speech engine failed to transcribe frame"
                    );

                    let tripped = cloud_state.trip(started_at, Instant::now(), CLOUD_RETRY_BACKOFF);

                    if tripped {
                        let notice = TranscriptionUpdate {
                            payload: UpdatePayload::Notice(SessionNotice {
                                level: NoticeLevel::Warn,
                                message: "云端识别异常，已回退至本地结果".to_string(),
                            }),
                            latency: frame_started.elapsed(),
                            frame_index,
                            is_first: false,
                        };

                        if let Err(err) = tx.send(notice).await {
                            warn!(
                                target: "engine_orchestrator",
                                %err,
                                "failed to deliver cloud fallback notice"
                            );
                        }
                    }
                }
            }
        });
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

#[cfg(feature = "local-asr")]
mod whisper {
    use super::*;
    use anyhow::anyhow;
    use std::mem::transmute;
    use std::path::Path;
    use std::sync::{Arc, Mutex};
    use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperState};

    pub struct WhisperLocalEngine {
        _context: Arc<WhisperContext>,
        streaming: Arc<Mutex<StreamingState>>,
    }

    impl WhisperLocalEngine {
        pub fn from_env() -> Result<Self> {
            let path = std::env::var("WHISPER_MODEL_PATH")?;
            Self::from_model_path(path)
        }

        pub fn from_model_path<P: AsRef<Path>>(path: P) -> Result<Self> {
            let path_ref = path.as_ref();
            let path_str = path_ref
                .to_str()
                .ok_or_else(|| anyhow!("模型路径不是有效的 UTF-8"))?;
            let context = Arc::new(WhisperContext::new(path_str)?);
            let state = unsafe {
                transmute::<WhisperState<'_>, WhisperState<'static>>(context.create_state()?)
            };
            Ok(Self {
                _context: Arc::clone(&context),
                streaming: Arc::new(Mutex::new(StreamingState::new(state))),
            })
        }
    }

    struct StreamingState {
        state: WhisperState<'static>,
        tail: Vec<f32>,
        pending: Vec<f32>,
        emitted: String,
        lookback_samples: usize,
        sample_rate: usize,
        min_stride_samples: usize,
        max_stride_samples: usize,
    }

    impl StreamingState {
        fn new(state: WhisperState<'static>) -> Self {
            const SAMPLE_RATE: usize = 16_000;
            const LOOKBACK_MS: usize = 240;
            const MIN_STRIDE_MS: usize = 80;
            const MAX_STRIDE_MS: usize = 200;
            let lookback_samples = (SAMPLE_RATE * LOOKBACK_MS + 999) / 1_000;
            let min_stride_samples = (SAMPLE_RATE * MIN_STRIDE_MS + 999) / 1_000;
            let max_stride_samples = (SAMPLE_RATE * MAX_STRIDE_MS + 999) / 1_000;
            Self {
                state,
                tail: Vec::with_capacity(lookback_samples),
                pending: Vec::with_capacity(max_stride_samples),
                emitted: String::new(),
                lookback_samples,
                sample_rate: SAMPLE_RATE,
                min_stride_samples,
                max_stride_samples,
            }
        }
    }

    #[async_trait]
    impl SpeechEngine for WhisperLocalEngine {
        async fn transcribe(&self, frame: &[f32]) -> Result<String> {
            if frame.is_empty() {
                return Ok(String::new());
            }

            let pcm: Vec<f32> = frame.to_vec();
            let speechy = frame_rms(frame) >= SPEECH_RMS_THRESHOLD;
            let streaming = Arc::clone(&self.streaming);

            tokio::task::spawn_blocking(move || {
                let mut guard = streaming
                    .lock()
                    .expect("whisper streaming state lock poisoned");

                guard.pending.extend_from_slice(&pcm);

                let should_decode = if speechy {
                    guard.pending.len() >= guard.min_stride_samples
                } else {
                    guard.pending.len() >= guard.max_stride_samples
                };

                if !should_decode {
                    return Ok(String::new());
                }

                let mut decode_window = Vec::with_capacity(guard.tail.len() + guard.pending.len());
                decode_window.extend_from_slice(&guard.tail);
                decode_window.extend_from_slice(&guard.pending);

                if decode_window.is_empty() {
                    return Ok(String::new());
                }

                let mut params = FullParams::new(SamplingStrategy::default());
                params.set_translate(false);
                params.set_single_segment(true);
                params.set_temperature(0.0);
                params.set_no_context(false);
                params.set_print_realtime(false);
                params.set_print_progress(false);

                let duration_ms = ((decode_window.len() * 1_000) / guard.sample_rate).max(1) as i32;
                params.set_duration_ms(duration_ms);

                guard.state.full(params, &decode_window)?;
                guard.pending.clear();

                let tail_len = guard.lookback_samples.min(decode_window.len());
                guard.tail = decode_window[decode_window.len() - tail_len..].to_vec();

                let mut transcript = String::new();
                let segments = guard.state.full_n_segments()? as usize;
                for segment in 0..segments {
                    let text = guard.state.full_get_segment_text(segment as i32)?;
                    let trimmed = text.trim();
                    if trimmed.is_empty() {
                        continue;
                    }
                    if !transcript.is_empty() {
                        transcript.push(' ');
                    }
                    transcript.push_str(trimmed);
                }

                let transcript = transcript.trim().to_string();
                if transcript.is_empty() {
                    return Ok(String::new());
                }

                let overlap = suffix_prefix_overlap(&guard.emitted, &transcript);
                let mut delta = transcript[overlap..].trim_start().to_string();

                if delta.is_empty() && guard.emitted.contains(&transcript) {
                    return Ok(String::new());
                }

                if !delta.is_empty() {
                    if !guard.emitted.is_empty()
                        && !guard.emitted.ends_with(' ')
                        && !delta.starts_with(' ')
                    {
                        guard.emitted.push(' ');
                    }
                    guard.emitted.push_str(&delta);
                } else {
                    guard.emitted = transcript.clone();
                    delta = transcript;
                }

                Ok(delta)
            })
            .await?
        }
    }
}

#[cfg(feature = "local-asr")]
use whisper::WhisperLocalEngine;

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::{anyhow, Result};
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Mutex, OnceLock};
    use tokio::time::timeout;

    fn env_guard() -> &'static Mutex<()> {
        static GUARD: OnceLock<Mutex<()>> = OnceLock::new();
        GUARD.get_or_init(|| Mutex::new(()))
    }

    struct MockSpeechEngine {
        segments: Mutex<VecDeque<String>>,
        delay: Duration,
    }

    impl MockSpeechEngine {
        fn new(segments: Vec<&str>, delay: Duration) -> Self {
            Self {
                segments: Mutex::new(segments.into_iter().map(String::from).collect()),
                delay,
            }
        }
    }

    #[async_trait]
    impl SpeechEngine for MockSpeechEngine {
        async fn transcribe(&self, _frame: &[f32]) -> Result<String> {
            sleep(self.delay).await;
            Ok(self
                .segments
                .lock()
                .expect("segments lock poisoned")
                .pop_front()
                .unwrap_or_default())
        }
    }

    struct SlowSecondLocalEngine {
        calls: AtomicUsize,
    }

    impl SlowSecondLocalEngine {
        fn new() -> Self {
            Self {
                calls: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait]
    impl SpeechEngine for SlowSecondLocalEngine {
        async fn transcribe(&self, _frame: &[f32]) -> Result<String> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            if call == 0 {
                sleep(Duration::from_millis(40)).await;
                Ok("local-fast".to_string())
            } else {
                sleep(Duration::from_millis(480)).await;
                Ok("local-slow".to_string())
            }
        }
    }

    #[test]
    fn fails_when_whisper_env_missing_without_fallback() {
        let _lock = env_guard().lock().expect("env guard poisoned");
        std::env::remove_var("WHISPER_MODEL_PATH");
        std::env::remove_var("WHISPER_ALLOW_FALLBACK");

        let result = EngineOrchestrator::new(EngineConfig {
            prefer_cloud: false,
        });

        assert!(
            result.is_err(),
            "expected whisper init failure without fallback"
        );
    }

    #[test]
    fn allows_fallback_when_explicitly_opted_in() {
        let _lock = env_guard().lock().expect("env guard poisoned");
        std::env::remove_var("WHISPER_MODEL_PATH");
        std::env::set_var("WHISPER_ALLOW_FALLBACK", "1");

        let orchestrator = EngineOrchestrator::new(EngineConfig {
            prefer_cloud: false,
        })
        .expect("fallback should be allowed when explicitly opted in");

        std::env::remove_var("WHISPER_ALLOW_FALLBACK");
        drop(orchestrator);
    }

    #[tokio::test]
    async fn emits_first_transcription_within_deadline() {
        let engine = Arc::new(MockSpeechEngine::new(
            vec!["hello", "world"],
            Duration::from_millis(50),
        ));
        let orchestrator = EngineOrchestrator::with_engine(
            EngineConfig {
                prefer_cloud: false,
            },
            engine,
        );

        let (session, mut rx) =
            orchestrator.start_realtime_session(RealtimeSessionConfig::default());

        // 100ms frame at 16kHz sample rate.
        let frame = vec![0.5_f32; 1_600];
        session
            .push_frame(frame)
            .await
            .expect("frame should enqueue");

        let update = timeout(Duration::from_millis(400), rx.recv())
            .await
            .expect("transcription timed out")
            .expect("channel closed unexpectedly");

        let payload = match update.payload {
            UpdatePayload::Transcript(payload) => payload,
            _ => panic!("expected transcript payload"),
        };
        assert_eq!(payload.text, "hello");
        assert_eq!(payload.source, TranscriptSource::Local);
        assert!(payload.is_primary);
        assert!(update.is_first);
        assert_eq!(update.frame_index, 1);
        assert!(update.latency <= Duration::from_millis(400));
    }

    struct FailingSpeechEngine;

    #[async_trait]
    impl SpeechEngine for FailingSpeechEngine {
        async fn transcribe(&self, _frame: &[f32]) -> Result<String> {
            sleep(Duration::from_millis(20)).await;
            Err(anyhow!("cloud unavailable"))
        }
    }

    #[tokio::test]
    async fn emits_deadline_notice_when_local_is_late() {
        let local_engine = Arc::new(MockSpeechEngine::new(
            vec!["local-slow"],
            Duration::from_millis(600),
        ));
        let cloud_engine = Arc::new(MockSpeechEngine::new(
            vec!["cloud-fast"],
            Duration::from_millis(40),
        ));

        let orchestrator = EngineOrchestrator::with_engines(
            EngineConfig {
                prefer_cloud: false,
            },
            local_engine,
            Some(cloud_engine),
        );

        let (session, mut rx) =
            orchestrator.start_realtime_session(RealtimeSessionConfig::default());

        let frame = vec![0.3_f32; 1_600];
        session
            .push_frame(frame.clone())
            .await
            .expect("frame should enqueue");

        let notice = timeout(Duration::from_millis(450), rx.recv())
            .await
            .expect("deadline notice timed out")
            .expect("channel closed unexpectedly");

        match notice.payload {
            UpdatePayload::Notice(session_notice) => {
                assert_eq!(session_notice.level, NoticeLevel::Warn);
                assert!(session_notice.message.contains("本地解码"));
            }
            _ => panic!("expected deadline notice"),
        }

        let (cloud_payload, cloud_is_first) = loop {
            let update = timeout(Duration::from_millis(700), rx.recv())
                .await
                .expect("cloud transcript timed out")
                .expect("channel closed unexpectedly");

            match update.payload {
                UpdatePayload::Transcript(payload) => break (payload, update.is_first),
                UpdatePayload::Notice(session_notice) => {
                    assert_eq!(session_notice.level, NoticeLevel::Warn);
                    assert!(session_notice.message.contains("本地解码"));
                }
            }
        };

        assert_eq!(cloud_payload.text, "cloud-fast");
        assert_eq!(cloud_payload.source, TranscriptSource::Cloud);
        assert!(cloud_payload.is_primary);
        assert!(!cloud_is_first);

        let local = timeout(Duration::from_millis(1_100), rx.recv())
            .await
            .expect("local transcript timed out")
            .expect("channel closed unexpectedly");

        match local.payload {
            UpdatePayload::Transcript(payload) => {
                assert_eq!(payload.text, "local-slow");
                assert_eq!(payload.source, TranscriptSource::Local);
                assert!(!payload.is_primary);
                assert!(!local.is_first);
            }
            _ => panic!("expected local transcript"),
        }
    }

    #[tokio::test]
    async fn silence_does_not_trigger_cadence_notice() {
        let engine = Arc::new(MockSpeechEngine::new(
            vec!["hello"],
            Duration::from_millis(40),
        ));
        let orchestrator = EngineOrchestrator::with_engine(
            EngineConfig {
                prefer_cloud: false,
            },
            engine,
        );

        let (session, mut rx) =
            orchestrator.start_realtime_session(RealtimeSessionConfig::default());

        let speech_frame = vec![0.5_f32; 1_600];
        session
            .push_frame(speech_frame)
            .await
            .expect("speech frame should enqueue");

        let update = timeout(Duration::from_millis(200), rx.recv())
            .await
            .expect("local transcript timed out")
            .expect("channel closed unexpectedly");

        match update.payload {
            UpdatePayload::Transcript(payload) => {
                assert_eq!(payload.text, "hello");
                assert_eq!(payload.source, TranscriptSource::Local);
                assert!(payload.is_primary);
            }
            _ => panic!("expected transcript payload"),
        }

        for _ in 0..5 {
            session
                .push_frame(vec![0.0_f32; 1_600])
                .await
                .expect("silent frame should enqueue");
        }

        assert!(
            timeout(Duration::from_millis(350), rx.recv())
                .await
                .is_err(),
            "silence triggered unexpected cadence notice",
        );
    }

    #[tokio::test]
    async fn emits_notice_when_local_engine_fails() {
        let local_engine = Arc::new(FailingSpeechEngine);
        let cloud_engine = Arc::new(MockSpeechEngine::new(
            vec!["cloud-ok"],
            Duration::from_millis(40),
        ));

        let orchestrator = EngineOrchestrator::with_engines(
            EngineConfig {
                prefer_cloud: false,
            },
            local_engine,
            Some(cloud_engine),
        );

        let (session, mut rx) =
            orchestrator.start_realtime_session(RealtimeSessionConfig::default());

        let frame = vec![0.3_f32; 1_600];
        session
            .push_frame(frame)
            .await
            .expect("frame should enqueue");

        let notice = timeout(Duration::from_millis(300), rx.recv())
            .await
            .expect("local failure notice timed out")
            .expect("channel closed unexpectedly");

        match notice.payload {
            UpdatePayload::Notice(session_notice) => {
                assert_eq!(session_notice.level, NoticeLevel::Error);
                assert!(session_notice.message.contains("本地识别异常"));
            }
            _ => panic!("expected local failure notice"),
        }

        let cloud = timeout(Duration::from_millis(600), rx.recv())
            .await
            .expect("cloud fallback timed out")
            .expect("channel closed unexpectedly");

        match cloud.payload {
            UpdatePayload::Transcript(payload) => {
                assert_eq!(payload.text, "cloud-ok");
                assert_eq!(payload.source, TranscriptSource::Cloud);
                assert!(payload.is_primary);
                assert!(!cloud.is_first);
            }
            _ => panic!("expected cloud transcript"),
        }

        let deadline_notice = timeout(Duration::from_millis(900), rx.recv())
            .await
            .expect("deadline notice timed out")
            .expect("channel closed unexpectedly");

        match deadline_notice.payload {
            UpdatePayload::Notice(session_notice) => {
                assert_eq!(session_notice.level, NoticeLevel::Warn);
                assert!(session_notice.message.contains("本地解码"));
            }
            _ => panic!("expected deadline degradation notice"),
        }
    }

    #[tokio::test]
    async fn cloud_waits_for_local_each_frame() {
        let local_engine = Arc::new(MockSpeechEngine::new(
            vec!["local-one", "local-two"],
            Duration::from_millis(120),
        ));
        let cloud_engine = Arc::new(MockSpeechEngine::new(
            vec!["cloud-one", "cloud-two"],
            Duration::from_millis(30),
        ));

        let orchestrator = EngineOrchestrator::with_engines(
            EngineConfig {
                prefer_cloud: false,
            },
            local_engine,
            Some(cloud_engine),
        );

        let (session, mut rx) =
            orchestrator.start_realtime_session(RealtimeSessionConfig::default());

        let frame = vec![0.4_f32; 1_600];
        session
            .push_frame(frame.clone())
            .await
            .expect("first frame should enqueue");

        let first = timeout(Duration::from_millis(400), rx.recv())
            .await
            .expect("first local transcript timed out")
            .expect("channel closed unexpectedly");

        match first.payload {
            UpdatePayload::Transcript(payload) => {
                assert_eq!(payload.text, "local-one");
                assert_eq!(payload.source, TranscriptSource::Local);
                assert!(first.is_first);
            }
            _ => panic!("expected first local transcript"),
        }

        let second = timeout(Duration::from_millis(600), rx.recv())
            .await
            .expect("cloud transcript timed out")
            .expect("channel closed unexpectedly");

        match second.payload {
            UpdatePayload::Transcript(payload) => {
                assert_eq!(payload.text, "cloud-one");
                assert_eq!(payload.source, TranscriptSource::Cloud);
                assert!(!second.is_first);
            }
            _ => panic!("expected first cloud transcript"),
        }

        session
            .push_frame(frame)
            .await
            .expect("second frame should enqueue");

        let third = timeout(Duration::from_millis(600), rx.recv())
            .await
            .expect("second local transcript timed out")
            .expect("channel closed unexpectedly");

        match third.payload {
            UpdatePayload::Transcript(payload) => {
                assert_eq!(payload.text, "local-two");
                assert_eq!(payload.source, TranscriptSource::Local);
                assert_eq!(third.frame_index, 2);
            }
            _ => panic!("expected second local transcript"),
        }

        let fourth = timeout(Duration::from_millis(800), rx.recv())
            .await
            .expect("second cloud transcript timed out")
            .expect("channel closed unexpectedly");

        match fourth.payload {
            UpdatePayload::Transcript(payload) => {
                assert_eq!(payload.text, "cloud-two");
                assert_eq!(payload.source, TranscriptSource::Cloud);
                assert_eq!(fourth.frame_index, 2);
                assert!(!fourth.is_first);
            }
            _ => panic!("expected second cloud transcript"),
        }
    }

    #[tokio::test]
    async fn cloud_does_not_preempt_when_updates_channel_full() {
        let local_engine = Arc::new(MockSpeechEngine::new(
            vec!["local-backpressure"],
            Duration::from_millis(120),
        ));
        let cloud_engine = Arc::new(MockSpeechEngine::new(
            vec!["cloud-fast"],
            Duration::from_millis(20),
        ));

        let orchestrator = EngineOrchestrator::with_engines(
            EngineConfig {
                prefer_cloud: false,
            },
            local_engine,
            Some(cloud_engine),
        );

        let (session, mut rx) = orchestrator.start_realtime_session(RealtimeSessionConfig {
            buffer_capacity: 1,
            ..RealtimeSessionConfig::default()
        });

        let updates_tx = session.updates_tx.clone();
        updates_tx
            .try_send(TranscriptionUpdate {
                payload: UpdatePayload::Notice(SessionNotice {
                    level: NoticeLevel::Info,
                    message: "prefill".to_string(),
                }),
                latency: Duration::from_millis(0),
                frame_index: 0,
                is_first: false,
            })
            .expect("prefill updates channel");

        let frame = vec![0.5_f32; 1_600];
        session
            .push_frame(frame)
            .await
            .expect("frame should enqueue");

        sleep(Duration::from_millis(150)).await;
        let progress = session.local_progress.clone();
        assert_eq!(progress.last_frame(), 0);

        let _ = timeout(Duration::from_millis(50), rx.recv())
            .await
            .expect("prefill frame not drained")
            .expect("channel closed unexpectedly");

        let first = timeout(Duration::from_millis(250), rx.recv())
            .await
            .expect("first transcript timed out")
            .expect("channel closed unexpectedly");

        match first.payload {
            UpdatePayload::Transcript(payload) => {
                assert_eq!(payload.text, "local-backpressure");
                assert_eq!(payload.source, TranscriptSource::Local);
                assert!(first.is_first);
            }
            _ => panic!("expected first local transcript"),
        }

        let second = timeout(Duration::from_millis(400), rx.recv())
            .await
            .expect("cloud transcript timed out")
            .expect("channel closed unexpectedly");

        match second.payload {
            UpdatePayload::Transcript(payload) => {
                assert_eq!(payload.text, "cloud-fast");
                assert_eq!(payload.source, TranscriptSource::Cloud);
                assert!(!second.is_first);
            }
            _ => panic!("expected cloud transcript after local"),
        }
    }

    #[tokio::test]
    async fn cadence_timeout_emits_notice_before_cloud_promotion() {
        let local_engine = Arc::new(SlowSecondLocalEngine::new());
        let cloud_engine = Arc::new(MockSpeechEngine::new(
            vec!["cloud-fast", "cloud-follow"],
            Duration::from_millis(50),
        ));

        let orchestrator = EngineOrchestrator::with_engines(
            EngineConfig {
                prefer_cloud: false,
            },
            local_engine,
            Some(cloud_engine),
        );

        let (session, mut rx) =
            orchestrator.start_realtime_session(RealtimeSessionConfig::default());

        let frame = vec![0.5_f32; 1_600];
        session
            .push_frame(frame.clone())
            .await
            .expect("first frame should enqueue");

        let first = timeout(Duration::from_millis(250), rx.recv())
            .await
            .expect("first local transcript timed out")
            .expect("channel closed unexpectedly");

        match first.payload {
            UpdatePayload::Transcript(payload) => {
                assert_eq!(payload.text, "local-fast");
                assert_eq!(payload.source, TranscriptSource::Local);
                assert!(first.is_first);
                assert!(payload.is_primary);
            }
            _ => panic!("expected first local transcript"),
        }

        let second = timeout(Duration::from_millis(500), rx.recv())
            .await
            .expect("first cloud transcript timed out")
            .expect("channel closed unexpectedly");

        match second.payload {
            UpdatePayload::Transcript(payload) => {
                assert_eq!(payload.text, "cloud-fast");
                assert_eq!(payload.source, TranscriptSource::Cloud);
                assert!(!second.is_first);
                assert!(!payload.is_primary);
            }
            _ => panic!("expected first cloud transcript"),
        }

        session
            .push_frame(frame)
            .await
            .expect("second frame should enqueue");

        let notice = timeout(Duration::from_millis(350), rx.recv())
            .await
            .expect("cadence notice timed out")
            .expect("channel closed unexpectedly");

        match notice.payload {
            UpdatePayload::Notice(session_notice) => {
                assert_eq!(session_notice.level, NoticeLevel::Warn);
                assert!(session_notice.message.contains("本地解码"));
            }
            _ => panic!("expected cadence fallback notice"),
        }

        let cloud = timeout(Duration::from_millis(650), rx.recv())
            .await
            .expect("fallback cloud transcript timed out")
            .expect("channel closed unexpectedly");

        match cloud.payload {
            UpdatePayload::Transcript(payload) => {
                assert_eq!(payload.text, "cloud-follow");
                assert_eq!(payload.source, TranscriptSource::Cloud);
                assert!(payload.is_primary);
                assert!(!cloud.is_first);
            }
            _ => panic!("expected fallback cloud transcript"),
        }

        let local = timeout(Duration::from_millis(950), rx.recv())
            .await
            .expect("delayed local transcript timed out")
            .expect("channel closed unexpectedly");

        match local.payload {
            UpdatePayload::Transcript(payload) => {
                assert_eq!(payload.text, "local-slow");
                assert_eq!(payload.source, TranscriptSource::Local);
                assert!(!payload.is_primary);
                assert!(!local.is_first);
            }
            _ => panic!("expected delayed local transcript"),
        }
    }

    #[tokio::test]
    async fn cloud_preferred_sessions_emit_local_first() {
        let local_engine = Arc::new(MockSpeechEngine::new(
            vec!["local-one"],
            Duration::from_millis(150),
        ));
        let cloud_engine = Arc::new(MockSpeechEngine::new(
            vec!["cloud-one"],
            Duration::from_millis(40),
        ));

        let orchestrator = EngineOrchestrator::with_engines(
            EngineConfig { prefer_cloud: true },
            local_engine,
            Some(cloud_engine),
        );

        let (session, mut rx) =
            orchestrator.start_realtime_session(RealtimeSessionConfig::default());

        let frame = vec![0.4_f32; 1_600];
        session
            .push_frame(frame.clone())
            .await
            .expect("first frame should enqueue");

        let first = timeout(Duration::from_millis(450), rx.recv())
            .await
            .expect("first local transcript timed out")
            .expect("channel closed unexpectedly");

        match first.payload {
            UpdatePayload::Transcript(payload) => {
                assert_eq!(payload.text, "local-one");
                assert_eq!(payload.source, TranscriptSource::Local);
                assert!(first.is_first);
                assert!(payload.is_primary);
            }
            _ => panic!("expected first local transcript"),
        }

        let second = timeout(Duration::from_millis(700), rx.recv())
            .await
            .expect("cloud transcript timed out")
            .expect("channel closed unexpectedly");

        match second.payload {
            UpdatePayload::Transcript(payload) => {
                assert_eq!(payload.text, "cloud-one");
                assert_eq!(payload.source, TranscriptSource::Cloud);
                assert!(!second.is_first);
                assert!(!payload.is_primary);
            }
            _ => panic!("expected cloud transcript"),
        }
    }

    #[tokio::test]
    async fn cloud_preferred_waits_for_whisper_each_frame() {
        let local_engine = Arc::new(MockSpeechEngine::new(
            vec!["local-one", "local-two"],
            Duration::from_millis(120),
        ));
        let cloud_engine = Arc::new(MockSpeechEngine::new(
            vec!["cloud-one", "cloud-two"],
            Duration::from_millis(30),
        ));

        let orchestrator = EngineOrchestrator::with_engines(
            EngineConfig { prefer_cloud: true },
            local_engine,
            Some(cloud_engine),
        );

        let (session, mut rx) =
            orchestrator.start_realtime_session(RealtimeSessionConfig::default());

        let frame = vec![0.4_f32; 1_600];
        session
            .push_frame(frame.clone())
            .await
            .expect("first frame should enqueue");

        let first = timeout(Duration::from_millis(450), rx.recv())
            .await
            .expect("first local transcript timed out")
            .expect("channel closed unexpectedly");

        match first.payload {
            UpdatePayload::Transcript(payload) => {
                assert_eq!(payload.text, "local-one");
                assert_eq!(payload.source, TranscriptSource::Local);
                assert!(first.is_first);
                assert!(payload.is_primary);
            }
            _ => panic!("expected first local transcript"),
        }

        let second = timeout(Duration::from_millis(700), rx.recv())
            .await
            .expect("cloud transcript timed out")
            .expect("channel closed unexpectedly");

        match second.payload {
            UpdatePayload::Transcript(payload) => {
                assert_eq!(payload.text, "cloud-one");
                assert_eq!(payload.source, TranscriptSource::Cloud);
                assert!(!second.is_first);
                assert!(!payload.is_primary);
            }
            _ => panic!("expected first cloud transcript"),
        }

        session
            .push_frame(frame)
            .await
            .expect("second frame should enqueue");

        let third = timeout(Duration::from_millis(600), rx.recv())
            .await
            .expect("second local transcript timed out")
            .expect("channel closed unexpectedly");

        match third.payload {
            UpdatePayload::Transcript(payload) => {
                assert_eq!(payload.text, "local-two");
                assert_eq!(payload.source, TranscriptSource::Local);
                assert_eq!(third.frame_index, 2);
                assert!(payload.is_primary);
            }
            _ => panic!("expected second local transcript"),
        }

        let fourth = timeout(Duration::from_millis(800), rx.recv())
            .await
            .expect("second cloud transcript timed out")
            .expect("channel closed unexpectedly");

        match fourth.payload {
            UpdatePayload::Transcript(payload) => {
                assert_eq!(payload.text, "cloud-two");
                assert_eq!(payload.source, TranscriptSource::Cloud);
                assert_eq!(fourth.frame_index, 2);
                assert!(!fourth.is_first);
                assert!(!payload.is_primary);
            }
            _ => panic!("expected second cloud transcript"),
        }
    }

    struct FlakyCloudEngine {
        attempts: AtomicUsize,
    }

    impl FlakyCloudEngine {
        fn new() -> Self {
            Self {
                attempts: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait]
    impl SpeechEngine for FlakyCloudEngine {
        async fn transcribe(&self, _frame: &[f32]) -> Result<String> {
            let call = self.attempts.fetch_add(1, Ordering::SeqCst);
            if call == 0 {
                sleep(Duration::from_millis(40)).await;
                Err(anyhow!("transient cloud outage"))
            } else {
                sleep(Duration::from_millis(60)).await;
                Ok("cloud-online".to_string())
            }
        }
    }

    #[tokio::test]
    async fn retries_cloud_after_backoff() {
        let local_engine = Arc::new(MockSpeechEngine::new(
            vec!["local-one", "local-two"],
            Duration::from_millis(15),
        ));
        let cloud_engine = Arc::new(FlakyCloudEngine::new());

        let orchestrator = EngineOrchestrator::with_engines(
            EngineConfig { prefer_cloud: true },
            local_engine,
            Some(cloud_engine),
        );

        let (session, mut rx) =
            orchestrator.start_realtime_session(RealtimeSessionConfig::default());

        let frame = vec![0.4_f32; 1_600];
        session
            .push_frame(frame.clone())
            .await
            .expect("frame should enqueue");

        let first = timeout(Duration::from_millis(400), rx.recv())
            .await
            .expect("first update timed out")
            .expect("channel closed unexpectedly");

        match first.payload {
            UpdatePayload::Transcript(payload) => {
                assert_eq!(payload.text, "local-one");
                assert_eq!(payload.source, TranscriptSource::Local);
                assert!(payload.is_primary);
            }
            _ => panic!("expected first local transcript"),
        }

        let notice = timeout(Duration::from_millis(600), rx.recv())
            .await
            .expect("fallback notice timed out")
            .expect("channel closed unexpectedly");

        match notice.payload {
            UpdatePayload::Notice(session_notice) => {
                assert_eq!(session_notice.level, NoticeLevel::Warn);
                assert!(session_notice.message.contains("云端识别异常"));
            }
            _ => panic!("expected fallback notice"),
        }

        sleep(CLOUD_RETRY_BACKOFF + Duration::from_millis(200)).await;

        session
            .push_frame(frame)
            .await
            .expect("frame should enqueue");

        let mut cloud_transcript = None;
        for _ in 0..4 {
            let update = timeout(Duration::from_millis(800), rx.recv())
                .await
                .expect("update timed out")
                .expect("channel closed unexpectedly");

            if let UpdatePayload::Transcript(payload) = &update.payload {
                if payload.source == TranscriptSource::Cloud {
                    cloud_transcript = Some((payload.clone(), update.is_first));
                    break;
                }
            }
        }

        let (cloud_payload, is_first) =
            cloud_transcript.expect("cloud transcript not received after retry");
        assert_eq!(cloud_payload.text, "cloud-online");
        assert!(!cloud_payload.is_primary);
        assert!(!is_first);
    }
    #[tokio::test]
    async fn emits_fallback_notice_when_cloud_engine_fails() {
        let local_engine = Arc::new(MockSpeechEngine::new(
            vec!["fallback"],
            Duration::from_millis(10),
        ));
        let cloud_engine = Arc::new(FailingSpeechEngine);

        let orchestrator = EngineOrchestrator::with_engines(
            EngineConfig { prefer_cloud: true },
            local_engine,
            Some(cloud_engine),
        );

        let (session, mut rx) =
            orchestrator.start_realtime_session(RealtimeSessionConfig::default());

        let frame = vec![0.5_f32; 1_600];
        session
            .push_frame(frame)
            .await
            .expect("frame should enqueue");

        let first = timeout(Duration::from_millis(400), rx.recv())
            .await
            .expect("transcription timed out")
            .expect("channel closed unexpectedly");

        let transcript = match first.payload {
            UpdatePayload::Transcript(payload) => payload,
            UpdatePayload::Notice(_) => panic!("expected transcript before notice"),
        };
        assert_eq!(transcript.text, "fallback");
        assert_eq!(transcript.source, TranscriptSource::Local);
        assert!(transcript.is_primary);

        let notice = timeout(Duration::from_millis(600), rx.recv())
            .await
            .expect("notice timed out")
            .expect("channel closed unexpectedly");

        match notice.payload {
            UpdatePayload::Notice(session_notice) => {
                assert_eq!(session_notice.level, NoticeLevel::Warn);
                assert!(session_notice.message.contains("云端识别异常"));
            }
            UpdatePayload::Transcript(_) => panic!("expected fallback notice"),
        }
    }

    #[tokio::test]
    async fn cloud_path_runs_when_local_is_primary() {
        let local_engine = Arc::new(MockSpeechEngine::new(
            vec!["local-first"],
            Duration::from_millis(5),
        ));
        let cloud_engine = Arc::new(FailingSpeechEngine);

        let orchestrator = EngineOrchestrator::with_engines(
            EngineConfig {
                prefer_cloud: false,
            },
            local_engine,
            Some(cloud_engine),
        );

        let (session, mut rx) =
            orchestrator.start_realtime_session(RealtimeSessionConfig::default());

        let frame = vec![0.5_f32; 1_600];
        session
            .push_frame(frame)
            .await
            .expect("frame should enqueue");

        let first = timeout(Duration::from_millis(400), rx.recv())
            .await
            .expect("transcription timed out")
            .expect("channel closed unexpectedly");

        let transcript = match first.payload {
            UpdatePayload::Transcript(payload) => payload,
            UpdatePayload::Notice(_) => panic!("expected transcript before notice"),
        };
        assert_eq!(transcript.text, "local-first");
        assert_eq!(transcript.source, TranscriptSource::Local);
        assert!(transcript.is_primary);

        let notice = timeout(Duration::from_millis(600), rx.recv())
            .await
            .expect("notice timed out")
            .expect("channel closed unexpectedly");

        match notice.payload {
            UpdatePayload::Notice(session_notice) => {
                assert_eq!(session_notice.level, NoticeLevel::Warn);
                assert!(session_notice.message.contains("云端识别异常"));
            }
            UpdatePayload::Transcript(_) => panic!("expected fallback notice"),
        }
    }
}
