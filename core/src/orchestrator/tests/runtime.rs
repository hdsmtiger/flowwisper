use crate::orchestrator::constants::CLOUD_RETRY_BACKOFF;
use crate::orchestrator::traits::LightweightSentencePolisher;
use crate::orchestrator::types::{SentenceSelection, SentenceVariant};
use crate::orchestrator::*;
use anyhow::{anyhow, Result};
use async_trait::async_trait;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tokio::time::{sleep, timeout};

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
    std::env::set_var("WHISPER_DISABLE_AUTO_DOWNLOAD", "1");

    let result = EngineOrchestrator::new(EngineConfig {
        prefer_cloud: false,
    });

    assert!(
        result.is_err(),
        "expected whisper init failure without fallback"
    );
    std::env::remove_var("WHISPER_DISABLE_AUTO_DOWNLOAD");
}

#[test]
fn allows_fallback_when_explicitly_opted_in() {
    let _lock = env_guard().lock().expect("env guard poisoned");
    std::env::remove_var("WHISPER_MODEL_PATH");
    std::env::set_var("WHISPER_ALLOW_FALLBACK", "1");
    std::env::set_var("WHISPER_DISABLE_AUTO_DOWNLOAD", "1");

    let orchestrator = EngineOrchestrator::new(EngineConfig {
        prefer_cloud: false,
    })
    .expect("fallback should be allowed when explicitly opted in");

    std::env::remove_var("WHISPER_ALLOW_FALLBACK");
    std::env::remove_var("WHISPER_DISABLE_AUTO_DOWNLOAD");
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

    let (session, mut rx) = orchestrator.start_realtime_session(RealtimeSessionConfig::default());

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

    let (session, mut rx) = orchestrator.start_realtime_session(RealtimeSessionConfig::default());

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

    let (session, mut rx) = orchestrator.start_realtime_session(RealtimeSessionConfig::default());

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

    let (session, mut rx) = orchestrator.start_realtime_session(RealtimeSessionConfig::default());

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

    let updates_tx = session.updates_sender();
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
    let progress = session.local_progress();
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
