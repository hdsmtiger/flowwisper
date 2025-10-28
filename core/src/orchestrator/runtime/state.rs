use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use crate::orchestrator::constants::{SILENCE_RMS_THRESHOLD, SPEECH_RMS_THRESHOLD};
use crate::orchestrator::types::{SentenceSelection, SentenceVariant};

use super::util::duration_to_ms;

#[derive(Default)]
pub(crate) struct LocalProgress {
    last_frame: AtomicU64,
    degraded: AtomicBool,
    last_update_ms: AtomicU64,
    speech_started_ms: AtomicU64,
    speech_active: AtomicBool,
}

impl LocalProgress {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn record_success(&self, frame_index: usize, started_at: Instant) {
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

    pub(crate) fn mark_degraded(&self, started_at: Instant) {
        self.degraded.store(true, Ordering::SeqCst);
        self.last_update_ms
            .store(duration_to_ms(started_at.elapsed()), Ordering::SeqCst);
    }

    pub(crate) fn record_frame_energy(&self, started_at: Instant, rms: f32) {
        if rms >= SPEECH_RMS_THRESHOLD {
            self.mark_speech_detected(started_at);
            self.speech_active.store(true, Ordering::SeqCst);
        } else if rms <= SILENCE_RMS_THRESHOLD {
            self.speech_active.store(false, Ordering::SeqCst);
        }
    }

    pub(crate) fn last_frame(&self) -> u64 {
        self.last_frame.load(Ordering::SeqCst)
    }

    pub(crate) fn is_degraded(&self) -> bool {
        self.degraded.load(Ordering::SeqCst)
    }

    pub(crate) fn last_update_ms(&self) -> u64 {
        self.last_update_ms.load(Ordering::SeqCst)
    }

    pub(crate) fn mark_speech_detected(&self, started_at: Instant) {
        let detected_ms = duration_to_ms(started_at.elapsed()).max(1);
        let _ = self.speech_started_ms.compare_exchange(
            0,
            detected_ms,
            Ordering::SeqCst,
            Ordering::SeqCst,
        );
    }

    pub(crate) fn speech_started_ms(&self) -> u64 {
        self.speech_started_ms.load(Ordering::SeqCst)
    }

    pub(crate) fn has_speech_started(&self) -> bool {
        self.speech_started_ms() != 0
    }

    pub(crate) fn is_speech_active(&self) -> bool {
        self.speech_active.load(Ordering::SeqCst)
    }
}

#[derive(Debug)]
pub(crate) struct LocalDecoderState {
    pub(crate) sentence_buffer: SentenceBuffer,
}

impl LocalDecoderState {
    pub(crate) fn new(window: Duration) -> Self {
        Self {
            sentence_buffer: SentenceBuffer::new(window),
        }
    }
}

#[derive(Debug)]
pub(crate) struct SentenceBuffer {
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

    pub(crate) fn ingest(&mut self, delta: &str, now: Instant) -> Vec<String> {
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

    pub(crate) fn take_completed_sentences(&mut self, now: Instant) -> Vec<String> {
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

#[derive(Debug, Default)]
pub(crate) struct SentenceStore {
    next_sentence_id: u64,
    records: BTreeMap<u64, SentenceRecord>,
}

#[derive(Debug)]
struct SentenceRecord {
    polished_text: Option<String>,
    polished_within_sla: Option<bool>,
    active_variant: SentenceVariant,
    user_override: bool,
}

impl SentenceStore {
    pub(crate) fn register_raw_sentence(&mut self) -> u64 {
        self.next_sentence_id = self.next_sentence_id.saturating_add(1);
        let sentence_id = self.next_sentence_id;
        let record = SentenceRecord {
            polished_text: None,
            polished_within_sla: None,
            active_variant: SentenceVariant::Raw,
            user_override: false,
        };
        self.records.insert(sentence_id, record);
        sentence_id
    }

    pub(crate) fn record_polished(
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

    pub(crate) fn apply_selection(
        &mut self,
        selections: &[SentenceSelection],
    ) -> Vec<SentenceSelection> {
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
