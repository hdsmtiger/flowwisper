//! 本地持久化层脚手架。

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::{mpsc, oneshot};
use tracing::info;

const DEFAULT_DRAFT_TITLE: &str = "Polished transcript";
const DEFAULT_DRAFT_TAG: &str = "transcript";
const MAX_DRAFT_HISTORY: usize = 240;
const MAX_NOTICE_HISTORY: usize = 240;

fn now_timestamp_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TranscriptRecord {
    pub session_id: String,
    pub raw_text: String,
    pub polished_text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DraftSaveRequest {
    pub draft_id: String,
    pub session_id: String,
    pub content: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DraftRecord {
    pub draft_id: String,
    pub session_id: String,
    pub title: String,
    pub tags: Vec<String>,
    pub content: String,
    pub created_at_ms: u128,
    pub updated_at_ms: u128,
}

impl DraftRecord {
    pub fn from_request(request: DraftSaveRequest) -> Self {
        let timestamp_ms = now_timestamp_ms();
        let title = request
            .title
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_DRAFT_TITLE.to_string());
        let tags = request
            .tags
            .unwrap_or_else(|| vec![DEFAULT_DRAFT_TAG.to_string()]);

        Self {
            draft_id: request.draft_id,
            session_id: request.session_id,
            title,
            tags,
            content: request.content,
            created_at_ms: timestamp_ms,
            updated_at_ms: timestamp_ms,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NoticeSaveRequest {
    pub notice_id: String,
    pub session_id: String,
    pub action: String,
    pub result: String,
    pub level: String,
    pub message: String,
    #[serde(default)]
    pub undo_token: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NoticeRecord {
    pub notice_id: String,
    pub session_id: String,
    pub action: String,
    pub result: String,
    pub level: String,
    pub message: String,
    pub undo_token: Option<String>,
    pub timestamp_ms: u128,
}

impl NoticeRecord {
    pub fn from_request(request: NoticeSaveRequest) -> Self {
        Self {
            notice_id: request.notice_id,
            session_id: request.session_id,
            action: request.action,
            result: request.result,
            level: request.level,
            message: request.message,
            undo_token: request.undo_token,
            timestamp_ms: now_timestamp_ms(),
        }
    }
}

#[derive(Debug)]
pub enum PersistenceCommand {
    StoreTranscript(TranscriptRecord),
    StoreDraft {
        record: DraftRecord,
        respond_to: oneshot::Sender<Result<DraftRecord>>,
    },
    StoreNotice {
        record: NoticeRecord,
        respond_to: oneshot::Sender<Result<NoticeRecord>>,
    },
    ListDrafts {
        limit: usize,
        respond_to: oneshot::Sender<Result<Vec<DraftRecord>>>,
    },
    ListNotices {
        limit: usize,
        respond_to: oneshot::Sender<Result<Vec<NoticeRecord>>>,
    },
}

#[derive(Clone)]
pub struct PersistenceHandle {
    tx: mpsc::Sender<PersistenceCommand>,
}

impl PersistenceHandle {
    pub fn new(tx: mpsc::Sender<PersistenceCommand>) -> Self {
        Self { tx }
    }

    pub async fn save_transcript(&self, record: TranscriptRecord) -> Result<()> {
        self.tx
            .send(PersistenceCommand::StoreTranscript(record))
            .await
            .map_err(|err| anyhow!("failed to queue transcript record: {err}"))
    }

    pub async fn save_draft(&self, request: DraftSaveRequest) -> Result<DraftRecord> {
        let record = DraftRecord::from_request(request);
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(PersistenceCommand::StoreDraft {
                record,
                respond_to: tx,
            })
            .await
            .map_err(|err| anyhow!("failed to queue draft save: {err}"))?;
        rx.await
            .map_err(|err| anyhow!("draft save channel closed: {err}"))?
    }

    pub async fn save_notice(&self, request: NoticeSaveRequest) -> Result<NoticeRecord> {
        let record = NoticeRecord::from_request(request);
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(PersistenceCommand::StoreNotice {
                record,
                respond_to: tx,
            })
            .await
            .map_err(|err| anyhow!("failed to queue notice save: {err}"))?;
        rx.await
            .map_err(|err| anyhow!("notice save channel closed: {err}"))?
    }

    pub async fn list_drafts(&self, limit: usize) -> Result<Vec<DraftRecord>> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(PersistenceCommand::ListDrafts {
                limit,
                respond_to: tx,
            })
            .await
            .map_err(|err| anyhow!("failed to queue draft list request: {err}"))?;
        rx.await
            .map_err(|err| anyhow!("draft list channel closed: {err}"))?
    }

    pub async fn list_notices(&self, limit: usize) -> Result<Vec<NoticeRecord>> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(PersistenceCommand::ListNotices {
                limit,
                respond_to: tx,
            })
            .await
            .map_err(|err| anyhow!("failed to queue notice list request: {err}"))?;
        rx.await
            .map_err(|err| anyhow!("notice list channel closed: {err}"))?
    }
}

pub struct PersistenceActor {
    rx: mpsc::Receiver<PersistenceCommand>,
    drafts: VecDeque<DraftRecord>,
    notices: VecDeque<NoticeRecord>,
}

impl PersistenceActor {
    pub fn new(rx: mpsc::Receiver<PersistenceCommand>) -> Self {
        Self {
            rx,
            drafts: VecDeque::with_capacity(MAX_DRAFT_HISTORY),
            notices: VecDeque::with_capacity(MAX_NOTICE_HISTORY),
        }
    }

    pub async fn run(mut self) -> Result<()> {
        while let Some(command) = self.rx.recv().await {
            match command {
                PersistenceCommand::StoreTranscript(record) => {
                    info!(target: "persistence", ?record, "received transcript record");
                    // TODO: 将记录写入 SQLCipher + FTS5。
                }
                PersistenceCommand::StoreDraft { record, respond_to } => {
                    let result = self.store_draft(record);
                    let _ = respond_to.send(result);
                }
                PersistenceCommand::StoreNotice { record, respond_to } => {
                    let result = self.store_notice(record);
                    let _ = respond_to.send(result);
                }
                PersistenceCommand::ListDrafts { limit, respond_to } => {
                    let result = Ok(self.collect_drafts(limit));
                    let _ = respond_to.send(result);
                }
                PersistenceCommand::ListNotices { limit, respond_to } => {
                    let result = Ok(self.collect_notices(limit));
                    let _ = respond_to.send(result);
                }
            }
        }
        Ok(())
    }

    fn store_draft(&mut self, record: DraftRecord) -> Result<DraftRecord> {
        info!(target: "persistence", draft_id = %record.draft_id, session_id = %record.session_id, "persisting transcript draft");
        Self::push_with_limit(&mut self.drafts, record.clone(), MAX_DRAFT_HISTORY);
        Ok(record)
    }

    fn store_notice(&mut self, record: NoticeRecord) -> Result<NoticeRecord> {
        info!(target: "persistence", notice_id = %record.notice_id, session_id = %record.session_id, action = %record.action, result = %record.result, "persisting publish notice");
        Self::push_with_limit(&mut self.notices, record.clone(), MAX_NOTICE_HISTORY);
        Ok(record)
    }

    fn collect_drafts(&self, limit: usize) -> Vec<DraftRecord> {
        let effective_limit = limit.min(self.drafts.len());
        self.drafts
            .iter()
            .rev()          // Reverse iterator (newest first)
            .take(effective_limit)  // Take the newest N items
            .cloned()
            .collect::<Vec<_>>()
            .into_iter()
            .rev()          // Reverse again (oldest first among the taken items)
            .collect()
    }

    fn collect_notices(&self, limit: usize) -> Vec<NoticeRecord> {
        let effective_limit = limit.min(self.notices.len());
        self.notices
            .iter()
            .rev()          // Reverse iterator (newest first)
            .take(effective_limit)  // Take the newest N items
            .cloned()
            .collect::<Vec<_>>()
            .into_iter()
            .rev()          // Reverse again (oldest first among the taken items)
            .collect()
    }

    fn push_with_limit<T>(deque: &mut VecDeque<T>, item: T, limit: usize) {
        if deque.len() >= limit {
            deque.pop_front();
        }
        deque.push_back(item);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn saves_draft_with_defaults_and_retrieves_history() {
        let (tx, rx) = mpsc::channel(4);
        let handle = PersistenceHandle::new(tx.clone());
        tokio::spawn(PersistenceActor::new(rx).run());

        let request = DraftSaveRequest {
            draft_id: "draft-1".into(),
            session_id: "session-1".into(),
            content: "Hello".into(),
            title: None,
            tags: None,
        };

        let record = handle
            .save_draft(request)
            .await
            .expect("draft save should succeed");

        assert_eq!(record.title, DEFAULT_DRAFT_TITLE);
        assert_eq!(record.tags, vec![DEFAULT_DRAFT_TAG.to_string()]);

        let history = handle
            .list_drafts(10)
            .await
            .expect("draft history should be returned");
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].draft_id, "draft-1");
    }

    #[tokio::test]
    async fn respects_draft_list_limit_and_order() {
        let (tx, rx) = mpsc::channel(4);
        let handle = PersistenceHandle::new(tx.clone());
        tokio::spawn(PersistenceActor::new(rx).run());

        for idx in 0..5 {
            let request = DraftSaveRequest {
                draft_id: format!("draft-{idx}"),
                session_id: "session".into(),
                content: format!("draft content {idx}"),
                title: Some(format!("Custom {idx}")),
                tags: Some(vec!["transcript".into(), idx.to_string()]),
            };

            handle
                .save_draft(request)
                .await
                .expect("draft save should succeed");
        }

        let history = handle
            .list_drafts(3)
            .await
            .expect("draft list should be returned");

        assert_eq!(history.len(), 3);
        assert_eq!(history[0].draft_id, "draft-2");
        assert_eq!(history[1].draft_id, "draft-3");
        assert_eq!(history[2].draft_id, "draft-4");
        assert_eq!(history[2].title, "Custom 4");
        assert_eq!(
            history[2].tags,
            vec!["transcript".to_string(), "4".to_string()]
        );
    }

    #[tokio::test]
    async fn stores_notices_and_limits_history() {
        let (tx, rx) = mpsc::channel(4);
        let handle = PersistenceHandle::new(tx.clone());
        tokio::spawn(PersistenceActor::new(rx).run());

        for idx in 0..(MAX_NOTICE_HISTORY + 5) {
            let request = NoticeSaveRequest {
                notice_id: format!("notice-{idx}"),
                session_id: "session".into(),
                action: "copy".into(),
                result: if idx % 2 == 0 {
                    "success".into()
                } else {
                    "failure".into()
                },
                level: "warn".into(),
                message: format!("notice #{idx}"),
                undo_token: None,
            };

            handle
                .save_notice(request)
                .await
                .expect("notice save should succeed");
        }

        let history = handle
            .list_notices(50)
            .await
            .expect("notice history should be returned");
        assert_eq!(history.len(), 50);
        assert_eq!(history.first().unwrap().notice_id, "notice-195");
        assert_eq!(
            history.last().unwrap().notice_id,
            format!("notice-{}", MAX_NOTICE_HISTORY + 4)
        );
    }
}
