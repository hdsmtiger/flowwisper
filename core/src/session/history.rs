//! Session history domain models and DTOs for persistence and IPC layers.

use serde::{Deserialize, Serialize};
use serde_json::json;
use std::cmp::min;

/// History retention in hours. Sessions older than this window will be purged.
pub const HISTORY_RETENTION_HOURS: i64 = 48;
/// Retention window expressed in milliseconds.
pub const HISTORY_RETENTION_MS: i64 = HISTORY_RETENTION_HOURS * 60 * 60 * 1_000;
/// Preview length surfaced in UI search results.
pub const HISTORY_PREVIEW_LIMIT: usize = 120;

/// Accuracy flag captured from user feedback flows.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AccuracyFlag {
    Accurate,
    InaccurateRaw,
    InaccuratePolished,
    Unknown,
}

impl Default for AccuracyFlag {
    fn default() -> Self {
        AccuracyFlag::Unknown
    }
}

impl AccuracyFlag {
    /// Returns the canonical string value persisted in the database.
    pub fn as_str(&self) -> &'static str {
        match self {
            AccuracyFlag::Accurate => "accurate",
            AccuracyFlag::InaccurateRaw => "inaccurate_raw",
            AccuracyFlag::InaccuratePolished => "inaccurate_polished",
            AccuracyFlag::Unknown => "unknown",
        }
    }

    pub fn from_db(value: Option<&str>) -> Self {
        match value {
            Some("accurate") => AccuracyFlag::Accurate,
            Some("inaccurate_raw") => AccuracyFlag::InaccurateRaw,
            Some("inaccurate_polished") => AccuracyFlag::InaccuratePolished,
            _ => AccuracyFlag::Unknown,
        }
    }
}

/// Post actions triggered from history detail (copy, reinsert, export, etc.).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HistoryActionKind {
    Copy,
    Reinsert,
    Export,
    SaveDraft,
    ClipboardBackup,
}

impl HistoryActionKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            HistoryActionKind::Copy => "copy",
            HistoryActionKind::Reinsert => "reinsert",
            HistoryActionKind::Export => "export",
            HistoryActionKind::SaveDraft => "save_draft",
            HistoryActionKind::ClipboardBackup => "clipboard_backup",
        }
    }
}

/// Metadata describing a user action taken on a history entry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HistoryPostAction {
    pub kind: HistoryActionKind,
    pub timestamp_ms: i64,
    #[serde(default = "HistoryPostAction::default_detail")]
    pub detail: serde_json::Value,
}

impl HistoryPostAction {
    fn default_detail() -> serde_json::Value {
        json!({})
    }

    pub fn clipboard_backup(timestamp_ms: i64) -> Self {
        Self {
            kind: HistoryActionKind::ClipboardBackup,
            timestamp_ms,
            detail: Self::default_detail(),
        }
    }
}

/// Snapshot of a completed session ready for persistence.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SessionSnapshot {
    pub session_id: String,
    pub started_at_ms: i64,
    pub completed_at_ms: i64,
    #[serde(default)]
    pub locale: Option<String>,
    #[serde(default)]
    pub app_identifier: Option<String>,
    #[serde(default)]
    pub app_version: Option<String>,
    #[serde(default)]
    pub confidence_score: Option<f32>,
    pub raw_transcript: String,
    pub polished_transcript: String,
    #[serde(default)]
    pub metadata: serde_json::Value,
    #[serde(default)]
    pub post_actions: Vec<HistoryPostAction>,
}

impl SessionSnapshot {
    pub fn duration_ms(&self) -> i64 {
        (self.completed_at_ms - self.started_at_ms).max(0)
    }

    pub fn expires_at_ms(&self) -> i64 {
        self.completed_at_ms + HISTORY_RETENTION_MS
    }

    pub fn preview(&self) -> String {
        let mut text = if !self.polished_transcript.trim().is_empty() {
            self.polished_transcript.clone()
        } else {
            self.raw_transcript.clone()
        };

        if text.len() > HISTORY_PREVIEW_LIMIT {
            let preview_end = min(text.len(), HISTORY_PREVIEW_LIMIT);
            text.truncate(preview_end);
            text.push_str("…");
        }

        text
    }
}

/// Query filters used when listing history entries.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct HistoryQuery {
    #[serde(default)]
    pub keyword: Option<String>,
    #[serde(default)]
    pub locale: Option<String>,
    #[serde(default)]
    pub app_identifier: Option<String>,
    #[serde(default = "HistoryQuery::default_limit")]
    pub limit: usize,
    #[serde(default)]
    pub offset: usize,
}

impl HistoryQuery {
    fn default_limit() -> usize {
        20
    }
}

/// History entry returned to callers.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HistoryEntry {
    pub session_id: String,
    pub started_at_ms: i64,
    pub completed_at_ms: i64,
    pub duration_ms: i64,
    #[serde(default)]
    pub locale: Option<String>,
    #[serde(default)]
    pub app_identifier: Option<String>,
    #[serde(default)]
    pub app_version: Option<String>,
    #[serde(default)]
    pub confidence_score: Option<f32>,
    pub raw_transcript: String,
    pub polished_transcript: String,
    pub preview: String,
    #[serde(default)]
    pub accuracy_flag: AccuracyFlag,
    #[serde(default)]
    pub accuracy_remarks: Option<String>,
    #[serde(default)]
    pub post_actions: Vec<HistoryPostAction>,
    #[serde(default)]
    pub metadata: serde_json::Value,
}

impl HistoryEntry {
    pub fn from_snapshot(snapshot: SessionSnapshot, accuracy: AccuracyFlag) -> Self {
        let preview = snapshot.preview();
        let SessionSnapshot {
            session_id,
            started_at_ms,
            completed_at_ms,
            locale,
            app_identifier,
            app_version,
            confidence_score,
            raw_transcript,
            polished_transcript,
            metadata,
            post_actions,
            ..
        } = snapshot;
        let duration_ms = (completed_at_ms - started_at_ms).max(0);
        Self {
            preview,
            accuracy_flag: accuracy,
            accuracy_remarks: None,
            post_actions,
            metadata,
            session_id,
            started_at_ms,
            completed_at_ms,
            duration_ms,
            locale,
            app_identifier,
            app_version,
            confidence_score,
            raw_transcript,
            polished_transcript,
        }
    }
}

/// Paginated result returned to UI/IPC clients.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct HistoryPage {
    pub entries: Vec<HistoryEntry>,
    pub next_offset: Option<usize>,
    pub total: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AccuracyUpdate {
    pub session_id: String,
    pub flag: AccuracyFlag,
    #[serde(default)]
    pub remarks: Option<String>,
}
