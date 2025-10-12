//! 音频采集与处理管线脚手架。

use anyhow::Result;
use bytes::Bytes;
use std::collections::VecDeque;
use std::convert::TryInto;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::time::Duration;
use tokio::sync::{broadcast, mpsc, Mutex as AsyncMutex, Notify};
use tokio::task;
use tokio::time::{interval, MissedTickBehavior};
use tracing::{info, warn};

const SAMPLE_RATE_HZ: u32 = 16_000;
const MIN_FRAME_MS: u64 = 100;
const MAX_FRAME_MS: u64 = 200;
const VAD_THRESHOLD: f32 = 1e-4;
const WAVEFORM_FRAME_MS: u64 = 32;

mod noise;
pub use noise::{NoiseDetector, NoiseEvent, SilenceCountdownStatus};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioCaptureStage {
    Idle,
    PreRoll,
    Recording,
}

#[derive(Clone)]
pub struct AudioPipeline {
    waveform_tx: broadcast::Sender<WaveformFrame>,
    pcm_subscribers: Arc<Mutex<Vec<PcmSubscriber>>>,
    min_frame_samples: usize,
    max_frame_samples: usize,
    pending: Arc<Mutex<VecDeque<f32>>>,
    waveform_frame_samples: usize,
    waveform_pending: Arc<Mutex<VecDeque<f32>>>,
    waveform_started: Arc<AtomicBool>,
    noise_tx: broadcast::Sender<NoiseEvent>,
    noise_detector: Arc<Mutex<NoiseDetector>>,
    stage: Arc<Mutex<AudioCaptureStage>>,
}

#[derive(Clone)]
struct PcmSubscriber {
    sender: mpsc::Sender<Arc<[f32]>>,
    state: Arc<AsyncMutex<SubscriberState>>,
    max_queue: usize,
    notify: Arc<Notify>,
    lossless: bool,
}

struct SubscriberState {
    queue: VecDeque<Arc<[f32]>>,
    active: bool,
}

impl PcmSubscriber {
    fn new(sender: mpsc::Sender<Arc<[f32]>>, max_queue: usize, lossless: bool) -> Self {
        Self {
            sender,
            state: Arc::new(AsyncMutex::new(SubscriberState {
                queue: VecDeque::new(),
                active: false,
            })),
            max_queue,
            notify: Arc::new(Notify::new()),
            lossless,
        }
    }

    fn is_closed(&self) -> bool {
        self.sender.is_closed()
    }

    async fn enqueue(&self, frame: Arc<[f32]>) {
        let mut state = self.state.lock().await;

        loop {
            if self.lossless && self.max_queue > 0 && state.queue.len() >= self.max_queue {
                let notify = Arc::clone(&self.notify);
                drop(state);
                notify.notified().await;
                state = self.state.lock().await;
                continue;
            } else if !self.lossless && self.max_queue > 0 && state.queue.len() >= self.max_queue {
                let _ = state.queue.pop_front();
                warn!(
                    target: "audio_pipeline",
                    max_queue = self.max_queue,
                    "pcm subscriber queue exceeded capacity; dropping oldest frame"
                );
                break;
            }
            break;
        }

        state.queue.push_back(frame);
        if state.active {
            return;
        }

        state.active = true;
        let state_arc = Arc::clone(&self.state);
        let sender = self.sender.clone();
        let notify = Arc::clone(&self.notify);
        drop(state);

        task::spawn(async move {
            loop {
                let next = {
                    let mut guard = state_arc.lock().await;
                    match guard.queue.pop_front() {
                        Some(frame) => frame,
                        None => {
                            guard.active = false;
                            notify.notify_waiters();
                            return;
                        }
                    }
                };

                if sender.send(next).await.is_err() {
                    let mut guard = state_arc.lock().await;
                    guard.queue.clear();
                    guard.active = false;
                    notify.notify_waiters();
                    warn!(
                        target: "audio_pipeline",
                        "pcm subscriber closed before frame delivery"
                    );
                    return;
                }

                notify.notify_waiters();
            }
        });
    }
}

#[derive(Clone, Debug)]
pub struct WaveformFrame {
    pub rms: f32,
    pub vad_active: bool,
}

impl AudioPipeline {
    fn spawn_waveform_scheduler(&self) {
        let pending = Arc::clone(&self.waveform_pending);
        let tx = self.waveform_tx.clone();
        let frame_samples = self.waveform_frame_samples;
        let started = Arc::clone(&self.waveform_started);

        task::spawn(async move {
            let mut ticker = interval(Duration::from_millis(WAVEFORM_FRAME_MS));
            ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);

            loop {
                ticker.tick().await;

                let maybe_chunk = {
                    let mut guard = pending.lock().expect("waveform accumulator poisoned");

                    if guard.len() >= frame_samples {
                        let chunk: Vec<f32> = guard.drain(..frame_samples).collect();
                        Some(chunk)
                    } else if started.load(Ordering::SeqCst) && !guard.is_empty() {
                        let mut chunk: Vec<f32> = guard.drain(..).collect();
                        chunk.resize(frame_samples, 0.0);
                        Some(chunk)
                    } else {
                        None
                    }
                };

                if let Some(chunk) = maybe_chunk {
                    let rms = frame_rms(&chunk);
                    let vad_active = rms >= VAD_THRESHOLD;
                    let _ = tx.send(WaveformFrame { rms, vad_active });
                } else if !started.load(Ordering::SeqCst) {
                    let _ = tx.send(WaveformFrame {
                        rms: 0.0,
                        vad_active: false,
                    });
                }
            }
        });
    }

    pub fn new() -> Self {
        let (waveform_tx, _) = broadcast::channel(32);
        let pcm_subscribers = Arc::new(Mutex::new(Vec::new()));
        let min_frame_samples =
            duration_to_samples(Duration::from_millis(MIN_FRAME_MS), SAMPLE_RATE_HZ);
        let max_frame_samples =
            duration_to_samples(Duration::from_millis(MAX_FRAME_MS), SAMPLE_RATE_HZ);
        let waveform_frame_samples =
            duration_to_samples(Duration::from_millis(WAVEFORM_FRAME_MS), SAMPLE_RATE_HZ);
        let (noise_tx, _) = broadcast::channel(32);
        let noise_detector = Arc::new(Mutex::new(NoiseDetector::new(SAMPLE_RATE_HZ)));
        let stage = Arc::new(Mutex::new(AudioCaptureStage::Idle));
        let pipeline = Self {
            waveform_tx,
            pcm_subscribers,
            min_frame_samples,
            max_frame_samples,
            pending: Arc::new(Mutex::new(VecDeque::new())),
            waveform_frame_samples,
            waveform_pending: Arc::new(Mutex::new(VecDeque::new())),
            waveform_started: Arc::new(AtomicBool::new(false)),
            noise_tx,
            noise_detector,
            stage,
        };

        pipeline.spawn_waveform_scheduler();

        pipeline
    }

    pub fn subscribe_waveform(&self) -> broadcast::Receiver<WaveformFrame> {
        self.waveform_tx.subscribe()
    }

    pub fn subscribe_noise_events(&self) -> broadcast::Receiver<NoiseEvent> {
        self.noise_tx.subscribe()
    }

    pub fn subscribe_pcm_frames(&self, capacity: usize) -> mpsc::Receiver<Arc<[f32]>> {
        self.subscribe_pcm_frames_with_options(capacity, false)
    }

    pub fn subscribe_lossless_pcm_frames(&self, capacity: usize) -> mpsc::Receiver<Arc<[f32]>> {
        self.subscribe_pcm_frames_with_options(capacity, true)
    }

    fn subscribe_pcm_frames_with_options(
        &self,
        capacity: usize,
        lossless: bool,
    ) -> mpsc::Receiver<Arc<[f32]>> {
        let bounded = capacity.max(1);
        let max_queue = if lossless {
            bounded
        } else {
            bounded.saturating_mul(4).max(bounded)
        };
        let (tx, rx) = mpsc::channel(bounded);
        let subscriber = PcmSubscriber::new(tx, max_queue, lossless);
        let mut guard = self
            .pcm_subscribers
            .lock()
            .expect("pcm subscriber registry poisoned");
        guard.push(subscriber);
        rx
    }

    pub async fn push_pcm_frame(&self, frame: Vec<f32>) -> Result<()> {
        if frame.is_empty() {
            return Ok(());
        }

        let chunks = {
            let mut guard = self.pending.lock().expect("pcm frame accumulator poisoned");
            guard.extend(frame);

            let mut chunks: Vec<Vec<f32>> = Vec::new();
            while guard.len() >= self.min_frame_samples {
                let chunk_len = guard.len().min(self.max_frame_samples);
                let chunk: Vec<f32> = guard.drain(0..chunk_len).collect();
                chunks.push(chunk);
            }

            chunks
        };

        for chunk in chunks {
            self.emit_chunk(chunk).await;
        }

        Ok(())
    }

    pub async fn flush_pending(&self) -> Result<()> {
        let chunks = {
            let mut guard = self.pending.lock().expect("pcm frame accumulator poisoned");

            if guard.is_empty() {
                return Ok(());
            }

            let mut chunks: Vec<Vec<f32>> = Vec::new();

            while guard.len() >= self.min_frame_samples {
                let chunk_len = guard.len().min(self.max_frame_samples);
                let chunk: Vec<f32> = guard.drain(0..chunk_len).collect();
                chunks.push(chunk);
            }

            if !guard.is_empty() {
                let mut tail: Vec<f32> = guard.drain(..).collect();
                if tail.len() < self.min_frame_samples {
                    tail.resize(self.min_frame_samples, 0.0);
                }
                chunks.push(tail);
            }

            chunks
        };

        for chunk in chunks {
            self.emit_chunk(chunk).await;
        }

        self.flush_waveform_tail();

        Ok(())
    }

    pub async fn start(&self) -> Result<()> {
        info!(target: "audio_pipeline", "starting placeholder pipeline");
        Ok(())
    }

    fn collect_subscribers(&self) -> Vec<PcmSubscriber> {
        let mut guard = self
            .pcm_subscribers
            .lock()
            .expect("pcm subscriber registry poisoned");
        guard.retain(|subscriber| !subscriber.is_closed());
        guard.iter().cloned().collect()
    }

    async fn emit_chunk(&self, chunk: Vec<f32>) {
        if chunk.is_empty() {
            return;
        }

        self.emit_waveform_samples(&chunk);
        self.process_noise_samples(&chunk);

        let shared: Arc<[f32]> = chunk.into();
        let subscribers = self.collect_subscribers();

        for subscriber in subscribers {
            subscriber.enqueue(Arc::clone(&shared)).await;
        }
    }

    fn emit_waveform_samples(&self, samples: &[f32]) {
        if samples.is_empty() {
            return;
        }

        let mut guard = self
            .waveform_pending
            .lock()
            .expect("waveform accumulator poisoned");
        guard.extend(samples.iter().copied());
        drop(guard);

        self.waveform_started.store(true, Ordering::SeqCst);
    }

    fn process_noise_samples(&self, samples: &[f32]) {
        if samples.is_empty() {
            return;
        }

        let stage = {
            let guard = self.stage.lock().expect("audio stage mutex poisoned");
            *guard
        };

        if matches!(stage, AudioCaptureStage::Idle) {
            return;
        }

        let events = {
            let mut detector = self
                .noise_detector
                .lock()
                .expect("noise detector mutex poisoned");
            detector.ingest(samples, stage)
        };

        self.dispatch_noise_events(events);
    }

    fn dispatch_noise_events(&self, events: Vec<NoiseEvent>) {
        for event in events {
            let _ = self.noise_tx.send(event);
        }
    }

    fn flush_waveform_tail(&self) {
        let mut guard = self
            .waveform_pending
            .lock()
            .expect("waveform accumulator poisoned");

        if guard.is_empty() {
            return;
        }

        let remainder = guard.len() % self.waveform_frame_samples;
        if remainder != 0 {
            let pad = self.waveform_frame_samples - remainder;
            for _ in 0..pad {
                guard.push_back(0.0);
            }
        }
    }

    pub async fn handle_frame(&self, _pcm: Bytes) -> Result<()> {
        // TODO: 接入 CoreAudio/WASAPI 并执行 VAD、降噪、增益控制。
        if _pcm.is_empty() {
            return Ok(());
        }

        if _pcm.len() % std::mem::size_of::<f32>() != 0 {
            warn!(
                target: "audio_pipeline",
                length = _pcm.len(),
                "pcm frame is not aligned to f32 samples"
            );
            return Ok(());
        }

        let mut frame = Vec::with_capacity(_pcm.len() / std::mem::size_of::<f32>());
        for chunk in _pcm.chunks_exact(4) {
            let bytes: [u8; 4] = chunk.try_into().expect("chunk size matches");
            frame.push(f32::from_le_bytes(bytes));
        }

        self.push_pcm_frame(frame).await?;
        Ok(())
    }

    pub fn begin_preroll(&self, baseline_db: Option<f32>) {
        {
            let mut stage = self.stage.lock().expect("audio stage mutex poisoned");
            *stage = AudioCaptureStage::PreRoll;
        }

        let events = {
            let mut detector = self
                .noise_detector
                .lock()
                .expect("noise detector mutex poisoned");
            detector.enter_preroll(baseline_db)
        };

        self.dispatch_noise_events(events);
    }

    pub fn begin_recording(&self) {
        {
            let mut stage = self.stage.lock().expect("audio stage mutex poisoned");
            *stage = AudioCaptureStage::Recording;
        }

        let mut detector = self
            .noise_detector
            .lock()
            .expect("noise detector mutex poisoned");
        detector.enter_recording();
    }

    pub fn reset_session(&self) {
        {
            let mut stage = self.stage.lock().expect("audio stage mutex poisoned");
            *stage = AudioCaptureStage::Idle;
        }

        let mut detector = self
            .noise_detector
            .lock()
            .expect("noise detector mutex poisoned");
        detector.reset();
    }
}

fn duration_to_samples(duration: Duration, sample_rate_hz: u32) -> usize {
    let samples = (duration.as_secs_f64() * sample_rate_hz as f64).round() as usize;
    samples.max(1)
}

fn frame_rms(frame: &[f32]) -> f32 {
    if frame.is_empty() {
        return 0.0;
    }

    let energy: f32 = frame.iter().map(|sample| sample * sample).sum();
    (energy / frame.len() as f32).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::{sleep, timeout, Duration};

    #[tokio::test]
    async fn slow_subscriber_does_not_block_realtime_feed() {
        let pipeline = AudioPipeline::new();
        let mut fast = pipeline.subscribe_pcm_frames(4);
        let slow = pipeline.subscribe_pcm_frames(1);

        let frame = vec![
            0.05_f32;
            duration_to_samples(Duration::from_millis(MIN_FRAME_MS), SAMPLE_RATE_HZ)
        ];

        pipeline
            .push_pcm_frame(frame.clone())
            .await
            .expect("first frame should succeed");

        let _ = timeout(Duration::from_millis(100), fast.recv())
            .await
            .expect("fast subscriber timed out")
            .expect("fast channel closed unexpectedly");

        // Intentionally avoid consuming from the slow subscriber so its bounded queue stays full.

        timeout(
            Duration::from_millis(100),
            pipeline.push_pcm_frame(frame.clone()),
        )
        .await
        .expect("push_pcm_frame stalled on slow subscriber")
        .expect("pipeline rejected frame");

        let received = timeout(Duration::from_millis(100), fast.recv())
            .await
            .expect("fast subscriber did not receive second frame")
            .expect("fast channel closed unexpectedly");
        assert_eq!(received.len(), frame.len());

        drop(slow);
        sleep(Duration::from_millis(10)).await;
    }

    #[tokio::test]
    async fn preserves_order_under_backpressure() {
        let pipeline = AudioPipeline::new();
        let mut rx = pipeline.subscribe_pcm_frames(2);

        let frame_len = duration_to_samples(Duration::from_millis(MIN_FRAME_MS), SAMPLE_RATE_HZ);

        for marker in 0..3 {
            let frame = vec![marker as f32; frame_len];
            pipeline
                .push_pcm_frame(frame)
                .await
                .expect("pushes frame under backpressure");
        }

        for expected in 0..3 {
            let received = timeout(Duration::from_millis(200), rx.recv())
                .await
                .expect("timed out waiting for ordered frame")
                .expect("channel closed unexpectedly");

            assert_eq!(received.len(), frame_len);
            assert!(received
                .iter()
                .all(|sample| (*sample - expected as f32).abs() < f32::EPSILON));
        }
    }

    #[tokio::test]
    async fn flushes_pending_tail_on_request() {
        let pipeline = AudioPipeline::new();
        let mut rx = pipeline.subscribe_pcm_frames(4);

        let half_frame =
            duration_to_samples(Duration::from_millis(MIN_FRAME_MS / 2), SAMPLE_RATE_HZ);
        pipeline
            .push_pcm_frame(vec![0.2_f32; half_frame])
            .await
            .expect("push half frame");

        // No frame should be emitted before the flush.
        assert!(timeout(Duration::from_millis(50), rx.recv()).await.is_err());

        pipeline.flush_pending().await.expect("flush pending audio");

        let flushed = timeout(Duration::from_millis(100), rx.recv())
            .await
            .expect("flush did not emit frame")
            .expect("channel closed unexpectedly");

        assert_eq!(
            flushed.len(),
            duration_to_samples(Duration::from_millis(MIN_FRAME_MS), SAMPLE_RATE_HZ)
        );
        assert!(flushed
            .iter()
            .take(half_frame)
            .all(|sample| (*sample - 0.2_f32).abs() < f32::EPSILON));
        assert!(flushed
            .iter()
            .skip(half_frame)
            .all(|sample| sample.abs() < f32::EPSILON));
    }

    #[tokio::test]
    async fn drops_oldest_frame_when_queue_is_full() {
        let pipeline = AudioPipeline::new();
        let mut rx = pipeline.subscribe_pcm_frames(1);

        let frame_len = duration_to_samples(Duration::from_millis(MIN_FRAME_MS), SAMPLE_RATE_HZ);

        for marker in 0..10 {
            pipeline
                .push_pcm_frame(vec![marker as f32; frame_len])
                .await
                .expect("push frame while subscriber is stalled");
        }

        sleep(Duration::from_millis(10)).await;

        let mut seen = Vec::new();
        loop {
            match timeout(Duration::from_millis(500), rx.recv()).await {
                Ok(Some(frame)) => {
                    assert_eq!(frame.len(), frame_len);
                    seen.push(frame[0]);
                }
                Ok(None) | Err(_) => break,
            }
        }

        assert!(!seen.is_empty(), "no frames observed after backlog");
        assert!(seen.len() < 10, "backlog did not shed frames: {:?}", seen);
        assert!(seen.len() <= 5, "unexpected backlog size: {:?}", seen);
        assert!(
            seen.windows(2).all(|w| w[0] <= w[1]),
            "frames not monotonic: {:?}",
            seen
        );
        assert_eq!(seen.last().copied(), Some(9.0_f32));
    }

    #[tokio::test]
    async fn waveform_runs_at_target_cadence() {
        let pipeline = AudioPipeline::new();
        let mut waveform_rx = pipeline.subscribe_waveform();

        let frame =
            vec![0.2_f32; duration_to_samples(Duration::from_millis(MIN_FRAME_MS), SAMPLE_RATE_HZ)];

        pipeline
            .push_pcm_frame(frame)
            .await
            .expect("pcm frame should enqueue");

        let mut received = 0;
        while received < 3 {
            let frame = timeout(Duration::from_millis(150), waveform_rx.recv())
                .await
                .expect("waveform frame timed out")
                .expect("waveform channel closed unexpectedly");
            assert!(frame.rms > 0.0);
            assert!(frame.vad_active);
            received += 1;
        }
    }

    #[tokio::test]
    async fn waveform_flush_emits_tail_frame() {
        let pipeline = AudioPipeline::new();
        let mut waveform_rx = pipeline.subscribe_waveform();

        let half_frame =
            duration_to_samples(Duration::from_millis(MIN_FRAME_MS / 2), SAMPLE_RATE_HZ);
        pipeline
            .push_pcm_frame(vec![0.05_f32; half_frame])
            .await
            .expect("partial frame should enqueue");

        let preroll = timeout(Duration::from_millis(80), waveform_rx.recv())
            .await
            .expect("waveform pre-roll missing")
            .expect("waveform channel closed unexpectedly");
        assert_eq!(preroll.rms, 0.0);
        assert!(!preroll.vad_active);

        pipeline
            .flush_pending()
            .await
            .expect("flush should succeed");

        let frame = timeout(Duration::from_millis(200), waveform_rx.recv())
            .await
            .expect("waveform frame not emitted after flush")
            .expect("waveform channel closed unexpectedly");
        assert!(frame.rms > 0.0);
        assert!(frame.vad_active);
    }

    #[tokio::test]
    async fn noise_baseline_event_emitted_after_sampling() {
        let pipeline = AudioPipeline::new();
        pipeline.begin_preroll(None);
        let mut noise_rx = pipeline.subscribe_noise_events();

        let frame = vec![0.1_f32; duration_to_samples(Duration::from_millis(500), SAMPLE_RATE_HZ)];

        pipeline
            .push_pcm_frame(frame)
            .await
            .expect("pcm frame should enqueue");

        let event = timeout(Duration::from_millis(200), noise_rx.recv())
            .await
            .expect("noise baseline event timed out")
            .expect("noise channel closed unexpectedly");

        match event {
            NoiseEvent::BaselineEstablished { level_db } => {
                assert!((level_db + 20.0).abs() < 1.5);
            }
            NoiseEvent::NoiseWarning(_) => {
                panic!("expected baseline event, received noise warning");
            }
            NoiseEvent::SilenceCountdown(_) => {
                panic!("expected baseline event, received silence countdown");
            }
        }
    }
}
