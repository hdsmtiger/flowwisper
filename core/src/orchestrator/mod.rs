//! 引擎编排服务脚手架。

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    Arc,
};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, Mutex, Notify};
use tokio::task::JoinHandle;
use tokio::time::{sleep, sleep_until, timeout, Instant as TokioInstant};
use tracing::{error, info, warn};

use crate::telemetry::events::{
    record_dual_view_latency, record_dual_view_revert, DualViewSelectionLog,
};

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

#[async_trait]
pub trait SentencePolisher: Send + Sync {
    async fn polish(&self, sentence: &str) -> Result<String>;
}

#[derive(Debug, Default)]
struct LightweightSentencePolisher;

impl LightweightSentencePolisher {
    fn normalize(sentence: &str) -> String {
        let trimmed = sentence.trim();
        if trimmed.is_empty() {
            return String::new();
        }

        let mut tokens: Vec<String> = trimmed
            .split_whitespace()
            .map(|token| token.trim_matches(|c: char| c.is_control()).to_string())
            .filter(|token| !token.is_empty())
            .collect();

        while tokens
            .first()
            .map(|token| Self::is_disfluency(token))
            .unwrap_or(false)
        {
            tokens.remove(0);
        }

        for token in tokens.iter_mut() {
            let lower = token.to_ascii_lowercase();
            match lower.as_str() {
                "i" => *token = "I".into(),
                "i'm" => *token = "I'm".into(),
                "i'd" => *token = "I'd".into(),
                "i've" => *token = "I've".into(),
                "i'll" => *token = "I'll".into(),
                _ => {}
            }
        }

        let mut text = tokens.join(" ");
        for mark in [",", ".", "!", "?", ";", ":"] {
            let pattern = format!(" {mark}");
            text = text.replace(&pattern, mark);
        }

        Self::capitalize_start(&mut text);

        if let Some(last) = text.chars().last() {
            if !matches!(last, '.' | '!' | '?' | '。' | '！' | '？' | '…') {
                text.push('.');
            }
        } else {
            text.push('.');
        }

        text
    }

    fn is_disfluency(token: &str) -> bool {
        matches!(
            token.to_ascii_lowercase().as_str(),
            "uh" | "um" | "erm" | "ah" | "eh" | "hmm"
        )
    }

    fn capitalize_start(text: &mut String) {
        let mut chars: Vec<char> = text.chars().collect();
        for ch in chars.iter_mut() {
            if ch.is_alphabetic() {
                if ch.is_lowercase() {
                    if let Some(upper) = ch.to_uppercase().next() {
                        *ch = upper;
                    }
                }
                break;
            }
        }
        *text = chars.into_iter().collect();
    }
}

#[async_trait]
impl SentencePolisher for LightweightSentencePolisher {
    async fn polish(&self, sentence: &str) -> Result<String> {
        Ok(Self::normalize(sentence))
    }
}

pub struct EngineOrchestrator {
    config: EngineConfig,
    local_engine: Arc<dyn SpeechEngine>,
    cloud_engine: Option<Arc<dyn SpeechEngine>>,
    polisher: Arc<dyn SentencePolisher>,
}

impl EngineOrchestrator {
    pub fn new(config: EngineConfig) -> Result<Self> {
        let local_engine = Self::build_local_engine()?;
        Ok(Self::with_components(
            config,
            local_engine,
            None,
            Arc::new(LightweightSentencePolisher::default()),
        ))
    }

    pub fn with_engine(config: EngineConfig, local_engine: Arc<dyn SpeechEngine>) -> Self {
        Self::with_components(
            config,
            local_engine,
            None,
            Arc::new(LightweightSentencePolisher::default()),
        )
    }

    pub fn with_engines(
        config: EngineConfig,
        local_engine: Arc<dyn SpeechEngine>,
        cloud_engine: Option<Arc<dyn SpeechEngine>>,
    ) -> Self {
        Self::with_components(
            config,
            local_engine,
            cloud_engine,
            Arc::new(LightweightSentencePolisher::default()),
        )
    }

    pub fn with_components(
        config: EngineConfig,
        local_engine: Arc<dyn SpeechEngine>,
        cloud_engine: Option<Arc<dyn SpeechEngine>>,
        polisher: Arc<dyn SentencePolisher>,
    ) -> Self {
        Self {
            config,
            local_engine,
            cloud_engine,
            polisher,
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
        let (command_tx, command_rx) = mpsc::channel(config.buffer_capacity);
        let first_update_flag = Arc::new(AtomicBool::new(false));
        let first_local_update_flag = Arc::new(AtomicBool::new(false));
        let local_progress = Arc::new(LocalProgress::new());
        let local_update_notify = Arc::new(Notify::new());
        let local_serial = Arc::new(Mutex::new(LocalDecoderState::new(config.raw_emit_window)));
        let sentences = Arc::new(Mutex::new(SentenceStore::default()));
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
            command_rx,
            tx.clone(),
            Arc::clone(&self.local_engine),
            self.cloud_engine.clone(),
            Arc::clone(&self.polisher),
            first_update_flag.clone(),
            first_local_update_flag.clone(),
            local_progress.clone(),
            local_update_notify.clone(),
            Arc::clone(&local_serial),
            Arc::clone(&sentences),
            started_at,
            self.config.prefer_cloud,
        );

        let handle = RealtimeSessionHandle {
            config,
            frame_tx,
            command_tx,
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

#[derive(Debug, Clone)]
pub enum UpdatePayload {
    Transcript(TranscriptPayload),
    Notice(SessionNotice),
    Selection(TranscriptSelectionPayload),
}

#[derive(Debug, Clone)]
pub struct TranscriptPayload {
    pub sentence_id: u64,
    pub text: String,
    pub source: TranscriptSource,
    pub is_primary: bool,
    pub within_sla: bool,
}

#[derive(Debug, Clone)]
pub struct TranscriptSelectionPayload {
    pub selections: Vec<SentenceSelection>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SentenceVariant {
    Raw,
    Polished,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SentenceSelection {
    pub sentence_id: u64,
    pub active_variant: SentenceVariant,
}

fn variant_label(variant: SentenceVariant) -> &'static str {
    match variant {
        SentenceVariant::Raw => "raw",
        SentenceVariant::Polished => "polished",
    }
}

#[derive(Debug, Clone)]
pub enum TranscriptCommand {
    ApplySelection(Vec<SentenceSelection>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranscriptSource {
    Local,
    Cloud,
    Polished,
}

impl TranscriptSource {
    fn as_str(&self) -> &'static str {
        match self {
            TranscriptSource::Local => "local",
            TranscriptSource::Cloud => "cloud",
            TranscriptSource::Polished => "polished",
        }
    }
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

#[derive(Debug)]
struct LocalDecoderState {
    sentence_buffer: SentenceBuffer,
}

impl LocalDecoderState {
    fn new(window: Duration) -> Self {
        Self {
            sentence_buffer: SentenceBuffer::new(window),
        }
    }
}

#[derive(Debug)]
struct SentenceBuffer {
    pending: String,
    pending_since: Option<Instant>,
    window: Duration,
}

impl SentenceBuffer {
    fn new(window: Duration) -> Self {
        Self {
            pending: String::new(),
            pending_since: None,
            window,
        }
    }

    fn ingest(&mut self, delta: &str, now: Instant) -> Vec<String> {
        let mut ready = Vec::new();
        let has_content = !delta.trim().is_empty();

        if has_content {
            let trimmed_start = if self.pending.is_empty() {
                delta.trim_start_matches(char::is_whitespace)
            } else {
                delta
            };

            if !self.pending.is_empty() && needs_injected_space(&self.pending, trimmed_start) {
                self.pending.push(' ');
            }

            self.pending.push_str(trimmed_start);

            if self.pending_since.is_none() && !self.pending.is_empty() {
                self.pending_since = Some(now);
            }

            ready.extend(self.take_completed_sentences(now));
        }

        if ready.is_empty() {
            if let Some(since) = self.pending_since {
                if now.saturating_duration_since(since) >= self.window && !self.pending.is_empty() {
                    ready.push(self.pending.trim().to_string());
                    self.pending.clear();
                    self.pending_since = None;
                }
            }
        }

        ready
    }

    fn take_completed_sentences(&mut self, now: Instant) -> Vec<String> {
        let mut ready = Vec::new();

        loop {
            let Some(boundary) = find_sentence_boundary(&self.pending) else {
                break;
            };

            let chunk = self.pending[..boundary].trim().to_string();
            if !chunk.is_empty() {
                ready.push(chunk);
            }

            let remainder = self.pending[boundary..]
                .trim_start_matches(char::is_whitespace)
                .to_string();
            self.pending = remainder;

            if self.pending.is_empty() {
                self.pending_since = None;
            } else {
                self.pending_since = Some(now);
            }
        }

        ready
    }
}

fn find_sentence_boundary(pending: &str) -> Option<usize> {
    let mut chars = pending.char_indices();
    while let Some((idx, ch)) = chars.next() {
        if !is_sentence_boundary(ch) {
            continue;
        }

        let mut boundary = idx + ch.len_utf8();
        while let Some(next) = pending[boundary..].chars().next() {
            if next == ch && is_sentence_boundary(next) {
                boundary += next.len_utf8();
            } else {
                break;
            }
        }

        return Some(boundary);
    }
    None
}

fn is_sentence_boundary(ch: char) -> bool {
    matches!(
        ch,
        '.' | '!' | '?' | '\n' | '\r' | '。' | '！' | '？' | '…' | ';' | '；'
    )
}

fn needs_injected_space(existing: &str, addition: &str) -> bool {
    let last = existing.chars().rev().find(|c| !c.is_whitespace());
    let first = addition.chars().find(|c| !c.is_whitespace());

    match (last, first) {
        (Some(l), Some(f)) => {
            !l.is_whitespace()
                && !f.is_whitespace()
                && !is_sentence_boundary(l)
                && !is_sentence_boundary(f)
                && !matches!(f, ',' | '，' | ':' | '：')
        }
        _ => false,
    }
}

#[derive(Debug, Default)]
struct SentenceStore {
    next_sentence_id: u64,
    records: BTreeMap<u64, SentenceRecord>,
}

#[derive(Debug)]
struct SentenceRecord {
    raw_text: String,
    raw_source: TranscriptSource,
    polished_text: Option<String>,
    polished_within_sla: Option<bool>,
    active_variant: SentenceVariant,
    user_override: bool,
}

impl SentenceStore {
    fn register_raw_sentence(&mut self, text: String, source: TranscriptSource) -> u64 {
        self.next_sentence_id = self.next_sentence_id.saturating_add(1);
        let sentence_id = self.next_sentence_id;
        let record = SentenceRecord {
            raw_text: text,
            raw_source: source,
            polished_text: None,
            polished_within_sla: None,
            active_variant: SentenceVariant::Raw,
            user_override: false,
        };
        self.records.insert(sentence_id, record);
        sentence_id
    }

    fn record_polished(
        &mut self,
        sentence_id: u64,
        text: String,
        within_sla: bool,
    ) -> Option<SentenceVariant> {
        if let Some(record) = self.records.get_mut(&sentence_id) {
            record.polished_text = Some(text);
            record.polished_within_sla = Some(within_sla);
            if !record.user_override {
                record.active_variant = SentenceVariant::Polished;
            }
            return Some(record.active_variant);
        }
        None
    }

    fn apply_selection(&mut self, selections: &[SentenceSelection]) -> Vec<SentenceSelection> {
        let mut applied = Vec::new();

        for selection in selections {
            if let Some(record) = self.records.get_mut(&selection.sentence_id) {
                match selection.active_variant {
                    SentenceVariant::Raw => {
                        record.active_variant = SentenceVariant::Raw;
                        record.user_override = true;
                        applied.push(*selection);
                    }
                    SentenceVariant::Polished => {
                        if record.polished_text.is_some() {
                            record.active_variant = SentenceVariant::Polished;
                            record.user_override = false;
                            applied.push(*selection);
                        }
                    }
                }
            }
        }

        applied
    }
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
    command_tx: mpsc::Sender<TranscriptCommand>,
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

    pub async fn apply_sentence_selections(
        &self,
        selections: Vec<SentenceSelection>,
    ) -> Result<(), mpsc::error::SendError<TranscriptCommand>> {
        if selections.is_empty() {
            return Ok(());
        }

        self.command_tx
            .send(TranscriptCommand::ApplySelection(selections))
            .await
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
    command_rx: mpsc::Receiver<TranscriptCommand>,
    updates_tx: mpsc::Sender<TranscriptionUpdate>,
    local_engine: Arc<dyn SpeechEngine>,
    cloud_engine: Option<Arc<dyn SpeechEngine>>,
    polisher: Arc<dyn SentencePolisher>,
    first_update_flag: Arc<AtomicBool>,
    first_local_update_flag: Arc<AtomicBool>,
    local_progress: Arc<LocalProgress>,
    local_update_notify: Arc<Notify>,
    local_serial: Arc<Mutex<LocalDecoderState>>,
    sentences: Arc<Mutex<SentenceStore>>,
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
        command_rx: mpsc::Receiver<TranscriptCommand>,
        updates_tx: mpsc::Sender<TranscriptionUpdate>,
        local_engine: Arc<dyn SpeechEngine>,
        cloud_engine: Option<Arc<dyn SpeechEngine>>,
        polisher: Arc<dyn SentencePolisher>,
        first_update_flag: Arc<AtomicBool>,
        first_local_update_flag: Arc<AtomicBool>,
        local_progress: Arc<LocalProgress>,
        local_update_notify: Arc<Notify>,
        local_serial: Arc<Mutex<LocalDecoderState>>,
        sentences: Arc<Mutex<SentenceStore>>,
        started_at: Instant,
        prefer_cloud: bool,
    ) -> Self {
        Self {
            config,
            frame_rx,
            command_rx,
            updates_tx,
            local_engine,
            cloud_engine,
            polisher,
            first_update_flag,
            first_local_update_flag,
            local_progress,
            local_update_notify,
            local_serial,
            sentences,
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
        let mut frame_closed = false;
        let mut command_closed = false;

        loop {
            if frame_closed && command_closed {
                break;
            }

            tokio::select! {
                biased;

                maybe_command = self.command_rx.recv(), if !command_closed => {
                    match maybe_command {
                        Some(command) => {
                            self.handle_command(command).await;
                        }
                        None => {
                            command_closed = true;
                        }
                    }
                }

                maybe_frame = self.frame_rx.recv(), if !frame_closed => {
                    match maybe_frame {
                        Some(frame) => {
                            frame_index += 1;

                            let frame_duration = Duration::from_secs_f64(
                                frame.len() as f64 / self.config.sample_rate_hz as f64,
                            );

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
                        None => {
                            frame_closed = true;
                        }
                    }
                }

                else => {
                    break;
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
        let sentences_store = self.sentences.clone();
        let started_at = self.started_at;
        let polisher = Arc::clone(&self.polisher);
        let polish_deadline = self.config.polish_emit_deadline;
        let polisher_enabled = self.config.enable_polisher;

        tokio::spawn(async move {
            let mut guard = local_serial.lock().await;
            match engine.transcribe(frame.as_ref()).await {
                Ok(text) => {
                    let now = Instant::now();
                    let sentences = guard.sentence_buffer.ingest(&text, now);
                    drop(guard);

                    if sentences.is_empty() {
                        return;
                    }

                    let claimed_first = first_flag
                        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                        .is_ok();
                    let was_first_local = !first_local_flag.load(Ordering::SeqCst);
                    let is_primary = !local_progress.is_degraded();
                    let mut emitted = false;
                    let mut first_emit = true;

                    for chunk in sentences {
                        let sentence_id = {
                            let mut store = sentences_store.lock().await;
                            store.register_raw_sentence(chunk.clone(), TranscriptSource::Local)
                        };
                        let polished_seed = chunk.clone();
                        let latency = frame_started.elapsed();
                        let update = TranscriptionUpdate {
                            payload: UpdatePayload::Transcript(TranscriptPayload {
                                sentence_id,
                                text: chunk,
                                source: TranscriptSource::Local,
                                is_primary,
                                within_sla: true,
                            }),
                            latency,
                            frame_index,
                            is_first: claimed_first && first_emit,
                        };

                        match tx.send(update).await {
                            Ok(_) => {
                                emitted = true;
                                record_dual_view_latency(
                                    sentence_id,
                                    variant_label(SentenceVariant::Raw),
                                    TranscriptSource::Local.as_str(),
                                    is_primary,
                                    latency,
                                    true,
                                );
                                if polisher_enabled {
                                    let polish_tx = tx.clone();
                                    let polisher = Arc::clone(&polisher);
                                    let sentences_store = sentences_store.clone();
                                    tokio::spawn(async move {
                                        let polish_started = Instant::now();
                                        match polisher.polish(&polished_seed).await {
                                            Ok(polished) => {
                                                let elapsed = polish_started.elapsed();
                                                let within_sla = elapsed <= polish_deadline;
                                                if !within_sla {
                                                    warn!(
                                                        target: "engine_orchestrator",
                                                        elapsed = ?elapsed,
                                                        deadline = ?polish_deadline,
                                                        "polished transcript exceeded deadline"
                                                    );
                                                }

                                                {
                                                    let mut store = sentences_store.lock().await;
                                                    store.record_polished(
                                                        sentence_id,
                                                        polished.clone(),
                                                        within_sla,
                                                    );
                                                }

                                                let update = TranscriptionUpdate {
                                                    payload: UpdatePayload::Transcript(
                                                        TranscriptPayload {
                                                            sentence_id,
                                                            text: polished,
                                                            source: TranscriptSource::Polished,
                                                            is_primary,
                                                            within_sla,
                                                        },
                                                    ),
                                                    latency: elapsed,
                                                    frame_index,
                                                    is_first: false,
                                                };

                                                match polish_tx.send(update).await {
                                                    Ok(_) => {
                                                        record_dual_view_latency(
                                                            sentence_id,
                                                            variant_label(
                                                                SentenceVariant::Polished,
                                                            ),
                                                            TranscriptSource::Polished.as_str(),
                                                            is_primary,
                                                            elapsed,
                                                            within_sla,
                                                        );
                                                    }
                                                    Err(err) => {
                                                        warn!(
                                                            target: "engine_orchestrator",
                                                            %err,
                                                            "failed to deliver polished transcript"
                                                        );
                                                    }
                                                }
                                            }
                                            Err(err) => {
                                                warn!(
                                                    target: "engine_orchestrator",
                                                    %err,
                                                    "failed to polish transcript sentence"
                                                );

                                                let notice = TranscriptionUpdate {
                                                    payload: UpdatePayload::Notice(SessionNotice {
                                                        level: NoticeLevel::Error,
                                                        message: "润色生成失败，已保留原始稿"
                                                            .to_string(),
                                                    }),
                                                    latency: polish_started.elapsed(),
                                                    frame_index,
                                                    is_first: false,
                                                };

                                                if let Err(err) = polish_tx.send(notice).await {
                                                    warn!(
                                                        target: "engine_orchestrator",
                                                        %err,
                                                        "failed to deliver polished error notice"
                                                    );
                                                }
                                            }
                                        }
                                    });
                                }
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
                                return;
                            }
                        }

                        first_emit = false;
                    }

                    if emitted {
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
                }
                Err(err) => {
                    drop(guard);
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
        let sentences_store = self.sentences.clone();

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
                    let sentence_id = {
                        let mut store = sentences_store.lock().await;
                        store.register_raw_sentence(text.clone(), TranscriptSource::Cloud)
                    };
                    let latency = frame_started.elapsed();
                    let is_primary = local_progress.is_degraded();
                    let update = TranscriptionUpdate {
                        payload: UpdatePayload::Transcript(TranscriptPayload {
                            sentence_id,
                            text,
                            source: TranscriptSource::Cloud,
                            is_primary,
                            within_sla: true,
                        }),
                        latency,
                        frame_index,
                        is_first,
                    };

                    match tx.send(update).await {
                        Ok(_) => {
                            record_dual_view_latency(
                                sentence_id,
                                variant_label(SentenceVariant::Raw),
                                TranscriptSource::Cloud.as_str(),
                                is_primary,
                                latency,
                                true,
                            );
                        }
                        Err(err) => {
                            warn!(
                                target: "engine_orchestrator",
                                %err,
                                "failed to deliver cloud transcription update"
                            );
                        }
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

    async fn handle_command(&self, command: TranscriptCommand) {
        match command {
            TranscriptCommand::ApplySelection(selections) => {
                if selections.is_empty() {
                    return;
                }

                let applied = {
                    let mut store = self.sentences.lock().await;
                    store.apply_selection(&selections)
                };

                let requested_log: Vec<DualViewSelectionLog> = selections
                    .iter()
                    .map(|selection| DualViewSelectionLog {
                        sentence_id: selection.sentence_id,
                        variant: variant_label(selection.active_variant),
                    })
                    .collect();
                let applied_log: Vec<DualViewSelectionLog> = applied
                    .iter()
                    .map(|selection| DualViewSelectionLog {
                        sentence_id: selection.sentence_id,
                        variant: variant_label(selection.active_variant),
                    })
                    .collect();

                record_dual_view_revert(requested_log, applied_log);

                if applied.is_empty() {
                    return;
                }

                let update = TranscriptionUpdate {
                    payload: UpdatePayload::Selection(TranscriptSelectionPayload {
                        selections: applied,
                    }),
                    latency: Duration::from_millis(0),
                    frame_index: 0,
                    is_first: false,
                };

                if let Err(err) = self.updates_tx.send(update).await {
                    warn!(
                        target: "engine_orchestrator",
                        %err,
                        "failed to deliver transcript selection acknowledgement"
                    );
                }
            }
        }
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

    #[tokio::test]
    async fn lightweight_polisher_applies_light_edits() {
        let polisher = LightweightSentencePolisher::default();
        let polished = polisher
            .polish("  uh i think i'm heading over around two  ")
            .await
            .expect("polish succeeds");
        assert_eq!(polished, "I think I'm heading over around two.");
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

    struct WindowSpeechEngine {
        segments: Mutex<VecDeque<&'static str>>,
        delay: Duration,
    }

    impl WindowSpeechEngine {
        fn new(segments: Vec<&'static str>, delay: Duration) -> Self {
            Self {
                segments: Mutex::new(segments.into_iter().collect()),
                delay,
            }
        }
    }

    #[async_trait]
    impl SpeechEngine for WindowSpeechEngine {
        async fn transcribe(&self, _frame: &[f32]) -> Result<String> {
            sleep(self.delay).await;
            Ok(self
                .segments
                .lock()
                .expect("segments lock poisoned")
                .pop_front()
                .unwrap_or_default()
                .to_string())
        }
    }

    struct SequencedSpeechEngine {
        segments: Mutex<VecDeque<(&'static str, Duration)>>,
    }

    impl SequencedSpeechEngine {
        fn new(segments: Vec<(&'static str, Duration)>) -> Self {
            Self {
                segments: Mutex::new(segments.into_iter().collect()),
            }
        }
    }

    #[async_trait]
    impl SpeechEngine for SequencedSpeechEngine {
        async fn transcribe(&self, _frame: &[f32]) -> Result<String> {
            let next = {
                let mut guard = self
                    .segments
                    .lock()
                    .expect("sequenced segments lock poisoned");
                guard.pop_front()
            };

            if let Some((text, delay)) = next {
                sleep(delay).await;
                Ok(text.to_string())
            } else {
                Ok(String::new())
            }
        }
    }

    struct SequencedPolisher {
        outputs: Mutex<VecDeque<(&'static str, Duration)>>,
    }

    impl SequencedPolisher {
        fn new(outputs: Vec<(&'static str, Duration)>) -> Self {
            Self {
                outputs: Mutex::new(outputs.into_iter().collect()),
            }
        }
    }

    #[async_trait]
    impl SentencePolisher for SequencedPolisher {
        async fn polish(&self, sentence: &str) -> Result<String> {
            let next = {
                let mut guard = self
                    .outputs
                    .lock()
                    .expect("sequenced polisher lock poisoned");
                guard.pop_front()
            };

            if let Some((text, delay)) = next {
                if !delay.is_zero() {
                    sleep(delay).await;
                }
                Ok(text.to_string())
            } else {
                Ok(sentence.to_string())
            }
        }
    }

    struct SlowPolisher {
        delay: Duration,
    }

    #[async_trait]
    impl SentencePolisher for SlowPolisher {
        async fn polish(&self, sentence: &str) -> Result<String> {
            sleep(self.delay).await;
            Ok(sentence.to_string())
        }
    }

    struct FailingPolisher;

    #[async_trait]
    impl SentencePolisher for FailingPolisher {
        async fn polish(&self, _sentence: &str) -> Result<String> {
            Err(anyhow!("polish failed"))
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
                Ok("local-fast.".to_string())
            } else {
                sleep(Duration::from_millis(480)).await;
                Ok("local-slow.".to_string())
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
            vec!["hello.", "world."],
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
        assert_eq!(payload.text, "hello.");
        assert_eq!(payload.source, TranscriptSource::Local);
        assert!(payload.is_primary);
        assert!(payload.within_sla);
        assert!(payload.sentence_id > 0);
        let sentence_id = payload.sentence_id;
        assert!(update.is_first);
        assert_eq!(update.frame_index, 1);
        assert!(update.latency <= Duration::from_millis(400));

        let polished = timeout(Duration::from_millis(500), rx.recv())
            .await
            .expect("polished transcript timed out")
            .expect("channel closed unexpectedly");

        match polished.payload {
            UpdatePayload::Transcript(polished_payload) => {
                assert_eq!(polished_payload.text, "Hello.");
                assert_eq!(polished_payload.source, TranscriptSource::Polished);
                assert!(polished_payload.is_primary);
                assert!(polished_payload.within_sla);
                assert_eq!(polished_payload.sentence_id, sentence_id);
            }
            _ => panic!("expected polished transcript payload"),
        }
        assert!(!polished.is_first);
        assert!(polished.latency <= Duration::from_millis(500));
    }

    #[tokio::test]
    async fn polished_transcript_marks_deadline_breach() {
        let local_engine = Arc::new(MockSpeechEngine::new(
            vec!["hello."],
            Duration::from_millis(20),
        ));
        let polisher: Arc<dyn SentencePolisher> = Arc::new(SlowPolisher {
            delay: Duration::from_millis(150),
        });

        let orchestrator = EngineOrchestrator::with_components(
            EngineConfig {
                prefer_cloud: false,
            },
            local_engine,
            None,
            polisher,
        );

        let mut config = RealtimeSessionConfig::default();
        config.polish_emit_deadline = Duration::from_millis(50);
        let (session, mut rx) = orchestrator.start_realtime_session(config);

        let frame = vec![0.5_f32; 1_600];
        session
            .push_frame(frame)
            .await
            .expect("frame should enqueue");

        // Drain the raw transcript first.
        let _ = timeout(Duration::from_millis(200), rx.recv())
            .await
            .expect("raw transcript timed out")
            .expect("channel closed unexpectedly");

        let polished = timeout(Duration::from_millis(400), rx.recv())
            .await
            .expect("polished transcript timed out")
            .expect("channel closed unexpectedly");

        match polished.payload {
            UpdatePayload::Transcript(payload) => {
                assert_eq!(payload.source, TranscriptSource::Polished);
                assert_eq!(payload.text, "hello.");
                assert!(!payload.within_sla);
            }
            _ => panic!("expected polished transcript payload"),
        }
        assert!(polished.latency >= Duration::from_millis(150));
        assert!(!polished.is_first);
    }

    #[tokio::test]
    async fn polisher_failure_emits_notice() {
        let local_engine = Arc::new(MockSpeechEngine::new(
            vec!["hi."],
            Duration::from_millis(20),
        ));
        let polisher: Arc<dyn SentencePolisher> = Arc::new(FailingPolisher);

        let orchestrator = EngineOrchestrator::with_components(
            EngineConfig {
                prefer_cloud: false,
            },
            local_engine,
            None,
            polisher,
        );

        let (session, mut rx) =
            orchestrator.start_realtime_session(RealtimeSessionConfig::default());

        let frame = vec![0.4_f32; 1_600];
        session
            .push_frame(frame)
            .await
            .expect("frame should enqueue");

        // Drain the raw transcript.
        let _ = timeout(Duration::from_millis(200), rx.recv())
            .await
            .expect("raw transcript timed out")
            .expect("channel closed unexpectedly");

        let notice = timeout(Duration::from_millis(300), rx.recv())
            .await
            .expect("polisher notice timed out")
            .expect("channel closed unexpectedly");

        match notice.payload {
            UpdatePayload::Notice(session_notice) => {
                assert_eq!(session_notice.level, NoticeLevel::Error);
                assert!(session_notice.message.contains("润色生成失败"));
            }
            _ => panic!("expected polishing failure notice"),
        }
    }

    #[tokio::test]
    async fn emits_selection_updates_on_revert_commands() {
        let engine = Arc::new(MockSpeechEngine::new(
            vec!["hello."],
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

        let frame = vec![0.5_f32; 1_600];
        session
            .push_frame(frame)
            .await
            .expect("frame should enqueue");

        let local = timeout(Duration::from_millis(400), rx.recv())
            .await
            .expect("local transcript timed out")
            .expect("channel closed unexpectedly");

        let sentence_id = match local.payload {
            UpdatePayload::Transcript(payload) => {
                assert_eq!(payload.source, TranscriptSource::Local);
                assert!(payload.is_primary);
                assert!(payload.sentence_id > 0);
                payload.sentence_id
            }
            other => panic!("expected local transcript, got {other:?}"),
        };

        let polished = timeout(Duration::from_millis(700), rx.recv())
            .await
            .expect("polished transcript timed out")
            .expect("channel closed unexpectedly");

        match polished.payload {
            UpdatePayload::Transcript(payload) => {
                assert_eq!(payload.source, TranscriptSource::Polished);
                assert_eq!(payload.sentence_id, sentence_id);
            }
            other => panic!("expected polished transcript, got {other:?}"),
        }

        session
            .apply_sentence_selections(vec![SentenceSelection {
                sentence_id,
                active_variant: SentenceVariant::Raw,
            }])
            .await
            .expect("revert command should be delivered");

        let selection = timeout(Duration::from_millis(500), rx.recv())
            .await
            .expect("selection update timed out")
            .expect("channel closed unexpectedly");

        match selection.payload {
            UpdatePayload::Selection(payload) => {
                assert_eq!(payload.selections.len(), 1);
                assert_eq!(payload.selections[0].sentence_id, sentence_id);
                assert_eq!(payload.selections[0].active_variant, SentenceVariant::Raw);
            }
            other => panic!("expected selection update, got {other:?}"),
        }

        session
            .apply_sentence_selections(vec![SentenceSelection {
                sentence_id,
                active_variant: SentenceVariant::Polished,
            }])
            .await
            .expect("restore command should be delivered");

        let restore = timeout(Duration::from_millis(500), rx.recv())
            .await
            .expect("restore update timed out")
            .expect("channel closed unexpectedly");

        match restore.payload {
            UpdatePayload::Selection(payload) => {
                assert_eq!(payload.selections.len(), 1);
                assert_eq!(payload.selections[0].sentence_id, sentence_id);
                assert_eq!(
                    payload.selections[0].active_variant,
                    SentenceVariant::Polished
                );
            }
            other => panic!("expected selection update, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn acknowledges_multi_sentence_revert_commands() {
        let local_engine = Arc::new(SequencedSpeechEngine::new(vec![
            ("first.", Duration::from_millis(35)),
            ("second.", Duration::from_millis(35)),
        ]));
        let polisher: Arc<dyn SentencePolisher> = Arc::new(SequencedPolisher::new(vec![
            ("First polished.", Duration::from_millis(25)),
            ("Second polished.", Duration::from_millis(25)),
        ]));

        let orchestrator = EngineOrchestrator::with_components(
            EngineConfig {
                prefer_cloud: false,
            },
            local_engine,
            None,
            polisher,
        );

        let (session, mut rx) =
            orchestrator.start_realtime_session(RealtimeSessionConfig::default());

        let frame = vec![0.4_f32; 1_600];
        session
            .push_frame(frame.clone())
            .await
            .expect("first frame should enqueue");
        session
            .push_frame(frame)
            .await
            .expect("second frame should enqueue");

        let mut sentence_ids = Vec::new();
        let mut polished_count = 0_u32;

        while sentence_ids.len() < 2 || polished_count < 2 {
            let update = timeout(Duration::from_millis(800), rx.recv())
                .await
                .expect("update timed out")
                .expect("channel closed unexpectedly");

            match update.payload {
                UpdatePayload::Transcript(payload) => match payload.source {
                    TranscriptSource::Local => {
                        sentence_ids.push(payload.sentence_id);
                    }
                    TranscriptSource::Polished => {
                        polished_count += 1;
                    }
                    TranscriptSource::Cloud => {}
                },
                UpdatePayload::Notice(_) => {}
                UpdatePayload::Selection(_) => {
                    panic!("unexpected selection payload before revert command");
                }
            }
        }

        let selections: Vec<SentenceSelection> = sentence_ids
            .iter()
            .map(|&sentence_id| SentenceSelection {
                sentence_id,
                active_variant: SentenceVariant::Raw,
            })
            .collect();

        session
            .apply_sentence_selections(selections.clone())
            .await
            .expect("revert command should be delivered");

        let acknowledgement = timeout(Duration::from_millis(500), rx.recv())
            .await
            .expect("selection acknowledgement timed out")
            .expect("channel closed unexpectedly");

        match acknowledgement.payload {
            UpdatePayload::Selection(payload) => {
                assert_eq!(payload.selections.len(), selections.len());
                for (expected, actual) in selections.iter().zip(payload.selections.iter()) {
                    assert_eq!(expected.sentence_id, actual.sentence_id);
                    assert_eq!(expected.active_variant, actual.active_variant);
                }
            }
            other => panic!("expected selection acknowledgement, got {other:?}"),
        }
        assert_eq!(acknowledgement.latency, Duration::from_millis(0));
        assert!(!acknowledgement.is_first);
    }

    #[tokio::test]
    async fn ignores_unknown_sentence_selection() {
        let local_engine = Arc::new(MockSpeechEngine::new(
            vec!["only."],
            Duration::from_millis(35),
        ));
        let orchestrator = EngineOrchestrator::with_engine(
            EngineConfig {
                prefer_cloud: false,
            },
            local_engine,
        );

        let mut config = RealtimeSessionConfig::default();
        config.enable_polisher = false;
        let (session, mut rx) = orchestrator.start_realtime_session(config);

        session
            .push_frame(vec![0.4_f32; 1_600])
            .await
            .expect("frame should enqueue");

        let update = timeout(Duration::from_millis(400), rx.recv())
            .await
            .expect("transcript timed out")
            .expect("channel closed unexpectedly");

        let sentence_id = match update.payload {
            UpdatePayload::Transcript(payload) => payload.sentence_id,
            other => panic!("expected transcript payload, got {other:?}"),
        };

        session
            .apply_sentence_selections(vec![SentenceSelection {
                sentence_id: sentence_id + 42,
                active_variant: SentenceVariant::Polished,
            }])
            .await
            .expect("selection command should be accepted");

        match timeout(Duration::from_millis(250), rx.recv()).await {
            Err(_) => {}
            Ok(Some(update)) => match update.payload {
                UpdatePayload::Selection(payload) => {
                    panic!("unexpected selection acknowledgement: {payload:?}")
                }
                _ => {}
            },
            Ok(None) => panic!("update channel closed unexpectedly"),
        }
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
            vec!["local-slow."],
            Duration::from_millis(600),
        ));
        let cloud_engine = Arc::new(MockSpeechEngine::new(
            vec!["cloud-fast."],
            Duration::from_millis(40),
        ));

        let orchestrator = EngineOrchestrator::with_engines(
            EngineConfig {
                prefer_cloud: false,
            },
            local_engine,
            Some(cloud_engine),
        );

        let mut config = RealtimeSessionConfig::default();
        config.enable_polisher = false;
        let (session, mut rx) = orchestrator.start_realtime_session(config);

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
                UpdatePayload::Selection(_) => {
                    panic!("unexpected selection update while waiting for cloud transcript");
                }
            }
        };

        assert_eq!(cloud_payload.text, "cloud-fast.");
        assert_eq!(cloud_payload.source, TranscriptSource::Cloud);
        assert!(cloud_payload.is_primary);
        assert!(!cloud_is_first);

        let local = timeout(Duration::from_millis(1_100), rx.recv())
            .await
            .expect("local transcript timed out")
            .expect("channel closed unexpectedly");

        match local.payload {
            UpdatePayload::Transcript(payload) => {
                assert_eq!(payload.text, "local-slow.");
                assert_eq!(payload.source, TranscriptSource::Local);
                assert!(!payload.is_primary);
                assert!(!local.is_first);
            }
            UpdatePayload::Notice(_) => panic!("expected local transcript"),
            UpdatePayload::Selection(_) => {
                panic!("unexpected selection update for local transcript")
            }
        }
    }

    #[tokio::test]
    async fn deadline_notice_latency_reflects_monitor() {
        let local_engine = Arc::new(MockSpeechEngine::new(
            vec!["local-slow."],
            Duration::from_millis(650),
        ));
        let cloud_engine = Arc::new(MockSpeechEngine::new(
            vec!["cloud-fast."],
            Duration::from_millis(40),
        ));

        let orchestrator = EngineOrchestrator::with_engines(
            EngineConfig {
                prefer_cloud: false,
            },
            local_engine,
            Some(cloud_engine),
        );

        let mut config = RealtimeSessionConfig::default();
        config.enable_polisher = false;
        config.first_update_deadline = Duration::from_millis(420);
        let (session, mut rx) = orchestrator.start_realtime_session(config);

        session
            .push_frame(vec![0.4_f32; 1_600])
            .await
            .expect("frame should enqueue");

        let notice = timeout(Duration::from_millis(520), rx.recv())
            .await
            .expect("deadline notice timed out")
            .expect("channel closed unexpectedly");

        assert!(notice.latency >= Duration::from_millis(380));
        assert!(notice.latency <= Duration::from_millis(520));
        match notice.payload {
            UpdatePayload::Notice(session_notice) => {
                assert_eq!(session_notice.level, NoticeLevel::Warn);
                assert!(session_notice.message.contains("本地解码"));
            }
            _ => panic!("expected deadline notice"),
        }

        drop(session);
    }

    #[tokio::test]
    async fn local_recovers_primary_after_cloud_fallback() {
        let local_engine = Arc::new(SequencedSpeechEngine::new(vec![
            ("local-slow.", Duration::from_millis(650)),
            ("local-recovered.", Duration::from_millis(60)),
        ]));
        let cloud_engine = Arc::new(SequencedSpeechEngine::new(vec![
            ("cloud-fallback.", Duration::from_millis(40)),
            ("cloud-secondary.", Duration::from_millis(120)),
        ]));

        let orchestrator = EngineOrchestrator::with_engines(
            EngineConfig {
                prefer_cloud: false,
            },
            local_engine,
            Some(cloud_engine),
        );

        let mut config = RealtimeSessionConfig::default();
        config.enable_polisher = false;
        let (session, mut rx) = orchestrator.start_realtime_session(config);

        let frame = vec![0.35_f32; 1_600];
        session
            .push_frame(frame.clone())
            .await
            .expect("first frame should enqueue");

        let notice = timeout(Duration::from_millis(500), rx.recv())
            .await
            .expect("deadline notice timed out")
            .expect("channel closed unexpectedly");

        match notice.payload {
            UpdatePayload::Notice(session_notice) => {
                assert_eq!(session_notice.level, NoticeLevel::Warn);
                assert!(session_notice.message.contains("本地解码"));
            }
            other => panic!("expected degradation notice, got {other:?}"),
        }

        let cloud_fallback = loop {
            let update = timeout(Duration::from_millis(700), rx.recv())
                .await
                .expect("cloud fallback timed out")
                .expect("channel closed unexpectedly");

            match update.payload {
                UpdatePayload::Transcript(payload) if payload.source == TranscriptSource::Cloud => {
                    break payload
                }
                UpdatePayload::Notice(_) => continue,
                UpdatePayload::Transcript(_) => continue,
                UpdatePayload::Selection(_) => {
                    panic!("unexpected selection before fallback transcript")
                }
            }
        };

        assert_eq!(cloud_fallback.text, "cloud-fallback.");
        assert!(cloud_fallback.is_primary);

        let delayed_local = loop {
            let update = timeout(Duration::from_millis(1_100), rx.recv())
                .await
                .expect("delayed local timed out")
                .expect("channel closed unexpectedly");

            match update.payload {
                UpdatePayload::Transcript(payload) if payload.source == TranscriptSource::Local => {
                    break payload
                }
                UpdatePayload::Notice(_) => continue,
                UpdatePayload::Selection(_) => {
                    panic!("unexpected selection before local recovery")
                }
                UpdatePayload::Transcript(_) => continue,
            }
        };

        assert_eq!(delayed_local.text, "local-slow.");
        assert!(!delayed_local.is_primary);

        session
            .push_frame(frame)
            .await
            .expect("second frame should enqueue");

        let recovered = loop {
            let update = timeout(Duration::from_millis(500), rx.recv())
                .await
                .expect("recovered local timed out")
                .expect("channel closed unexpectedly");

            match update.payload {
                UpdatePayload::Transcript(payload)
                    if payload.source == TranscriptSource::Local && update.frame_index == 2 =>
                {
                    break payload
                }
                UpdatePayload::Notice(_) => continue,
                UpdatePayload::Selection(_) => {
                    panic!("unexpected selection during recovery")
                }
                UpdatePayload::Transcript(_) => continue,
            }
        };

        assert_eq!(recovered.text, "local-recovered.");
        assert!(recovered.is_primary);

        let trailing_cloud = loop {
            let update = timeout(Duration::from_millis(800), rx.recv())
                .await
                .expect("cloud secondary timed out")
                .expect("channel closed unexpectedly");

            match update.payload {
                UpdatePayload::Transcript(payload) if payload.source == TranscriptSource::Cloud => {
                    break payload
                }
                UpdatePayload::Notice(_) => continue,
                UpdatePayload::Selection(_) => {
                    panic!("unexpected selection while waiting for trailing cloud")
                }
                UpdatePayload::Transcript(_) => continue,
            }
        };

        assert_eq!(trailing_cloud.text, "cloud-secondary.");
        assert!(!trailing_cloud.is_primary);
    }

    #[tokio::test]
    async fn silence_does_not_trigger_cadence_notice() {
        let engine = Arc::new(MockSpeechEngine::new(
            vec!["hello."],
            Duration::from_millis(40),
        ));
        let orchestrator = EngineOrchestrator::with_engine(
            EngineConfig {
                prefer_cloud: false,
            },
            engine,
        );

        let mut config = RealtimeSessionConfig::default();
        config.enable_polisher = false;
        let (session, mut rx) = orchestrator.start_realtime_session(config);

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
                assert_eq!(payload.text, "hello.");
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
            vec!["cloud-ok."],
            Duration::from_millis(40),
        ));

        let orchestrator = EngineOrchestrator::with_engines(
            EngineConfig {
                prefer_cloud: false,
            },
            local_engine,
            Some(cloud_engine),
        );

        let mut config = RealtimeSessionConfig::default();
        config.enable_polisher = false;
        let (session, mut rx) = orchestrator.start_realtime_session(config);

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
                assert_eq!(payload.text, "cloud-ok.");
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
            vec!["local-one.", "local-two."],
            Duration::from_millis(120),
        ));
        let cloud_engine = Arc::new(MockSpeechEngine::new(
            vec!["cloud-one.", "cloud-two."],
            Duration::from_millis(30),
        ));

        let orchestrator = EngineOrchestrator::with_engines(
            EngineConfig {
                prefer_cloud: false,
            },
            local_engine,
            Some(cloud_engine),
        );

        let mut config = RealtimeSessionConfig::default();
        config.enable_polisher = false;
        let (session, mut rx) = orchestrator.start_realtime_session(config);

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
                assert_eq!(payload.text, "local-one.");
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
                assert_eq!(payload.text, "cloud-one.");
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
                assert_eq!(payload.text, "local-two.");
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
                assert_eq!(payload.text, "cloud-two.");
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
            vec!["local-backpressure."],
            Duration::from_millis(120),
        ));
        let cloud_engine = Arc::new(MockSpeechEngine::new(
            vec!["cloud-fast."],
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
            enable_polisher: false,
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
                assert_eq!(payload.text, "local-backpressure.");
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
                assert_eq!(payload.text, "cloud-fast.");
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
            vec!["cloud-fast.", "cloud-follow."],
            Duration::from_millis(50),
        ));

        let orchestrator = EngineOrchestrator::with_engines(
            EngineConfig {
                prefer_cloud: false,
            },
            local_engine,
            Some(cloud_engine),
        );

        let mut config = RealtimeSessionConfig::default();
        config.enable_polisher = false;
        let (session, mut rx) = orchestrator.start_realtime_session(config);

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
                assert_eq!(payload.text, "local-fast.");
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
                assert_eq!(payload.text, "cloud-fast.");
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
                assert_eq!(payload.text, "cloud-follow.");
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
                assert_eq!(payload.text, "local-slow.");
                assert_eq!(payload.source, TranscriptSource::Local);
                assert!(!payload.is_primary);
                assert!(!local.is_first);
            }
            _ => panic!("expected delayed local transcript"),
        }
    }

    #[tokio::test]
    async fn flushes_partial_sentence_when_window_elapses() {
        let local_engine = Arc::new(WindowSpeechEngine::new(
            vec!["hello", "world"],
            Duration::from_millis(250),
        ));
        let orchestrator = EngineOrchestrator::with_engine(
            EngineConfig {
                prefer_cloud: false,
            },
            local_engine,
        );

        let (session, mut rx) = orchestrator.start_realtime_session(RealtimeSessionConfig {
            min_frame_duration: Duration::from_millis(50),
            max_frame_duration: Duration::from_millis(50),
            first_update_deadline: Duration::from_secs(5),
            raw_emit_window: Duration::from_millis(200),
            enable_polisher: false,
            ..RealtimeSessionConfig::default()
        });

        let frame = vec![0.4_f32; 800];
        session
            .push_frame(frame.clone())
            .await
            .expect("first frame should enqueue");
        session
            .push_frame(frame)
            .await
            .expect("second frame should enqueue");

        let update = timeout(Duration::from_millis(900), rx.recv())
            .await
            .expect("windowed transcript timed out")
            .expect("channel closed unexpectedly");

        match update.payload {
            UpdatePayload::Transcript(payload) => {
                assert_eq!(payload.text, "hello world");
                assert_eq!(payload.source, TranscriptSource::Local);
                assert!(payload.is_primary);
            }
            _ => panic!("expected transcript payload after window flush"),
        }
        assert!(update.is_first);
        assert!(update.latency >= Duration::from_millis(200));
    }

    #[tokio::test]
    async fn cloud_preferred_sessions_emit_local_first() {
        let local_engine = Arc::new(MockSpeechEngine::new(
            vec!["local-one."],
            Duration::from_millis(150),
        ));
        let cloud_engine = Arc::new(MockSpeechEngine::new(
            vec!["cloud-one."],
            Duration::from_millis(40),
        ));

        let orchestrator = EngineOrchestrator::with_engines(
            EngineConfig { prefer_cloud: true },
            local_engine,
            Some(cloud_engine),
        );

        let mut config = RealtimeSessionConfig::default();
        config.enable_polisher = false;
        let (session, mut rx) = orchestrator.start_realtime_session(config);

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
                assert_eq!(payload.text, "local-one.");
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
                assert_eq!(payload.text, "cloud-one.");
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
            vec!["local-one.", "local-two."],
            Duration::from_millis(120),
        ));
        let cloud_engine = Arc::new(MockSpeechEngine::new(
            vec!["cloud-one.", "cloud-two."],
            Duration::from_millis(30),
        ));

        let orchestrator = EngineOrchestrator::with_engines(
            EngineConfig { prefer_cloud: true },
            local_engine,
            Some(cloud_engine),
        );

        let mut config = RealtimeSessionConfig::default();
        config.enable_polisher = false;
        let (session, mut rx) = orchestrator.start_realtime_session(config);

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
                assert_eq!(payload.text, "local-one.");
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
                assert_eq!(payload.text, "cloud-one.");
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
                assert_eq!(payload.text, "local-two.");
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
                assert_eq!(payload.text, "cloud-two.");
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
                Ok("cloud-online.".to_string())
            }
        }
    }

    #[tokio::test]
    async fn retries_cloud_after_backoff() {
        let local_engine = Arc::new(MockSpeechEngine::new(
            vec!["local-one.", "local-two."],
            Duration::from_millis(15),
        ));
        let cloud_engine = Arc::new(FlakyCloudEngine::new());

        let orchestrator = EngineOrchestrator::with_engines(
            EngineConfig { prefer_cloud: true },
            local_engine,
            Some(cloud_engine),
        );

        let mut config = RealtimeSessionConfig::default();
        config.enable_polisher = false;
        let (session, mut rx) = orchestrator.start_realtime_session(config);

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
                assert_eq!(payload.text, "local-one.");
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
        assert_eq!(cloud_payload.text, "cloud-online.");
        assert!(!cloud_payload.is_primary);
        assert!(!is_first);
    }
    #[tokio::test]
    async fn emits_fallback_notice_when_cloud_engine_fails() {
        let local_engine = Arc::new(MockSpeechEngine::new(
            vec!["fallback."],
            Duration::from_millis(10),
        ));
        let cloud_engine = Arc::new(FailingSpeechEngine);

        let orchestrator = EngineOrchestrator::with_engines(
            EngineConfig { prefer_cloud: true },
            local_engine,
            Some(cloud_engine),
        );

        let mut config = RealtimeSessionConfig::default();
        config.enable_polisher = false;
        let (session, mut rx) = orchestrator.start_realtime_session(config);

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
            UpdatePayload::Selection(_) => panic!("unexpected selection before notice"),
        };
        assert_eq!(transcript.text, "fallback.");
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
            UpdatePayload::Selection(_) => {
                panic!("unexpected selection instead of fallback notice")
            }
        }
    }

    #[tokio::test]
    async fn cloud_path_runs_when_local_is_primary() {
        let local_engine = Arc::new(MockSpeechEngine::new(
            vec!["local-first."],
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

        let mut config = RealtimeSessionConfig::default();
        config.enable_polisher = false;
        let (session, mut rx) = orchestrator.start_realtime_session(config);

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
            UpdatePayload::Selection(_) => panic!("unexpected selection before notice"),
        };
        assert_eq!(transcript.text, "local-first.");
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
            UpdatePayload::Selection(_) => {
                panic!("unexpected selection instead of fallback notice")
            }
        }
    }
}
