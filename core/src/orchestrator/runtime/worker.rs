use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
};
use std::time::{Duration, Instant};

use crate::orchestrator::config::RealtimeSessionConfig;
use crate::orchestrator::constants::CLOUD_RETRY_BACKOFF;
use crate::orchestrator::traits::{SentencePolisher, SpeechEngine};
use crate::orchestrator::types::{
    variant_label, NoticeLevel, SentenceVariant, SessionNotice, TranscriptCommand,
    TranscriptPayload, TranscriptSelectionPayload, TranscriptSource, TranscriptionUpdate,
    UpdatePayload,
};
use crate::telemetry::events::{
    record_dual_view_latency, record_dual_view_revert, DualViewSelectionLog,
};
use tokio::sync::{mpsc, Mutex, Notify};
use tokio::task::JoinHandle;
use tokio::time::{sleep_until, timeout, Instant as TokioInstant};
use tracing::{error, warn};

use super::state::{LocalDecoderState, LocalProgress, SentenceStore};

pub(crate) struct RealtimeWorker {
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

        let elapsed_ms = super::util::duration_to_ms(now.saturating_duration_since(started_at));
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
        let elapsed_ms = super::util::duration_to_ms(now.saturating_duration_since(started_at));
        let backoff_ms = super::util::duration_to_ms(backoff);
        let next_retry = elapsed_ms.saturating_add(backoff_ms);
        self.next_retry_ms.store(next_retry, Ordering::SeqCst);
        let was_enabled = self.enabled.swap(false, Ordering::SeqCst);
        was_enabled
    }
}

fn frame_rms(frame: &[f32]) -> f32 {
    if frame.is_empty() {
        return 0.0;
    }

    let energy: f32 = frame.iter().map(|sample| sample * sample).sum();
    (energy / frame.len() as f32).sqrt()
}

impl RealtimeWorker {
    pub(crate) fn new(
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

    pub(crate) fn spawn(self) -> JoinHandle<()> {
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
                            store.register_raw_sentence()
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
                        store.register_raw_sentence()
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
