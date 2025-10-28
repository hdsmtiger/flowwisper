mod handle;
mod state;
mod util;
mod worker;

pub use handle::RealtimeSessionHandle;

use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::orchestrator::config::RealtimeSessionConfig;
use crate::orchestrator::traits::{SentencePolisher, SpeechEngine};
use crate::orchestrator::types::{NoticeLevel, SessionNotice, TranscriptionUpdate, UpdatePayload};
use tokio::sync::{mpsc, Mutex, Notify};
use tokio::task::JoinHandle;
use tokio::time::sleep;
use tracing::warn;

use self::state::{LocalDecoderState, LocalProgress, SentenceStore};
use self::worker::RealtimeWorker;

pub(crate) fn spawn_session(
    config: RealtimeSessionConfig,
    local_engine: Arc<dyn SpeechEngine>,
    cloud_engine: Option<Arc<dyn SpeechEngine>>,
    polisher: Arc<dyn SentencePolisher>,
    prefer_cloud: bool,
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

            let elapsed_ms = util::duration_to_ms(started_at.elapsed());
            let last_update_ms = monitor_progress.last_update_ms();
            let since_ms = elapsed_ms.saturating_sub(last_update_ms);
            let cadence_ms = util::duration_to_ms(cadence);

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
        Arc::clone(&local_engine),
        cloud_engine,
        polisher,
        first_update_flag.clone(),
        first_local_update_flag.clone(),
        local_progress.clone(),
        local_update_notify.clone(),
        Arc::clone(&local_serial),
        Arc::clone(&sentences),
        started_at,
        prefer_cloud,
    );

    let worker_handle = worker.spawn();
    let handle = RealtimeSessionHandle::new(
        config,
        frame_tx,
        command_tx,
        tx,
        local_progress,
        monitor,
        worker_handle,
    );

    (handle, rx)
}
