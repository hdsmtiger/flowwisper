use serde::{Deserialize, Serialize};
use std::time::Duration;

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

pub(crate) fn variant_label(variant: SentenceVariant) -> &'static str {
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
    pub fn as_str(&self) -> &'static str {
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
