//! 本地持久化层脚手架，负责编排 SQLCipher 数据库操作与回退逻辑。

pub mod sqlite;

use crate::persistence::sqlite::{SqliteConfig, SqlitePersistence};
use crate::session::history::{
    AccuracyUpdate, HistoryEntry, HistoryPage, HistoryPostAction, HistoryQuery, SessionSnapshot,
};
use crate::telemetry::events::{
    record_session_history_accuracy, record_session_history_action, record_session_history_cleanup,
    record_session_history_persist_failure, record_session_history_persisted,
};
use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value as JsonValue};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::{mpsc, oneshot};
use tokio::time::{sleep, timeout};
use tracing::{info, warn};

const DEFAULT_DRAFT_TITLE: &str = "Polished transcript";
const DEFAULT_DRAFT_TAG: &str = "transcript";
const MAX_DRAFT_HISTORY: usize = 240;
const MAX_NOTICE_HISTORY: usize = 240;
const PERSISTENCE_TIMEOUT_MS: u64 = 200;
const PERSISTENCE_RETRIES: u8 = 3;

fn now_timestamp_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
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
    PersistSession {
        snapshot: SessionSnapshot,
        respond_to: oneshot::Sender<Result<()>>,
    },
    SearchHistory {
        query: HistoryQuery,
        respond_to: oneshot::Sender<Result<HistoryPage>>,
    },
    UpdateAccuracy {
        update: AccuracyUpdate,
        respond_to: oneshot::Sender<Result<()>>,
    },
    AppendPostAction {
        session_id: String,
        action: HistoryPostAction,
        respond_to: oneshot::Sender<Result<Vec<HistoryPostAction>>>,
    },
    CleanupExpired {
        now_ms: i64,
        respond_to: oneshot::Sender<Result<usize>>,
    },
    EnqueueTelemetry {
        session_id: String,
        event_type: String,
        payload: JsonValue,
    },
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
    sqlite: Arc<SqlitePersistence>,
}

impl PersistenceHandle {
    pub fn new(tx: mpsc::Sender<PersistenceCommand>, sqlite: Arc<SqlitePersistence>) -> Self {
        Self { tx, sqlite }
    }

    pub fn database_path(&self) -> Option<PathBuf> {
        self.sqlite.database_path().map(|path| path.to_path_buf())
    }

    pub async fn persist_session(&self, snapshot: SessionSnapshot) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(PersistenceCommand::PersistSession {
                snapshot,
                respond_to: tx,
            })
            .await
            .map_err(|err| anyhow!("failed to queue session persistence: {err}"))?;
        rx.await
            .map_err(|err| anyhow!("persistence channel dropped: {err}"))?
    }

    pub async fn search_history(&self, query: HistoryQuery) -> Result<HistoryPage> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(PersistenceCommand::SearchHistory {
                query,
                respond_to: tx,
            })
            .await
            .map_err(|err| anyhow!("failed to queue history search: {err}"))?;
        rx.await
            .map_err(|err| anyhow!("history search channel dropped: {err}"))?
    }

    pub async fn load_session(&self, session_id: String) -> Result<Option<HistoryEntry>> {
        let sqlite = self.sqlite.clone();
        tokio::task::spawn_blocking(move || sqlite.load_session(&session_id))
            .await
            .map_err(|err| anyhow!("blocking load task failed: {err}"))?
    }

    pub async fn update_accuracy(&self, update: AccuracyUpdate) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(PersistenceCommand::UpdateAccuracy {
                update,
                respond_to: tx,
            })
            .await
            .map_err(|err| anyhow!("failed to queue accuracy update: {err}"))?;
        rx.await
            .map_err(|err| anyhow!("accuracy update channel dropped: {err}"))?
    }

    pub async fn append_post_action(
        &self,
        session_id: String,
        action: HistoryPostAction,
    ) -> Result<Vec<HistoryPostAction>> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(PersistenceCommand::AppendPostAction {
                session_id,
                action,
                respond_to: tx,
            })
            .await
            .map_err(|err| anyhow!("failed to queue post action: {err}"))?;
        rx.await
            .map_err(|err| anyhow!("post action channel dropped: {err}"))?
    }

    pub async fn enqueue_telemetry(
        &self,
        session_id: String,
        event_type: String,
        payload: JsonValue,
    ) -> Result<()> {
        self.tx
            .send(PersistenceCommand::EnqueueTelemetry {
                session_id,
                event_type,
                payload,
            })
            .await
            .map_err(|err| anyhow!("failed to queue telemetry payload: {err}"))
    }

    pub async fn cleanup_expired(&self, now_ms: i64) -> Result<usize> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(PersistenceCommand::CleanupExpired {
                now_ms,
                respond_to: tx,
            })
            .await
            .map_err(|err| anyhow!("failed to queue cleanup job: {err}"))?;
        rx.await
            .map_err(|err| anyhow!("cleanup channel dropped: {err}"))?
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
            .map_err(|err| anyhow!("draft save channel dropped: {err}"))?
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
            .map_err(|err| anyhow!("notice save channel dropped: {err}"))?
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
            .map_err(|err| anyhow!("draft list channel dropped: {err}"))?
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
            .map_err(|err| anyhow!("notice list channel dropped: {err}"))?
    }
}

pub struct PersistenceActor {
    rx: mpsc::Receiver<PersistenceCommand>,
    drafts: VecDeque<DraftRecord>,
    notices: VecDeque<NoticeRecord>,
    sqlite: Arc<SqlitePersistence>,
}

impl PersistenceActor {
    pub fn new(sqlite: Arc<SqlitePersistence>, rx: mpsc::Receiver<PersistenceCommand>) -> Self {
        Self {
            rx,
            drafts: VecDeque::with_capacity(MAX_DRAFT_HISTORY),
            notices: VecDeque::with_capacity(MAX_NOTICE_HISTORY),
            sqlite,
        }
    }

    pub async fn run(mut self) -> Result<()> {
        while let Some(command) = self.rx.recv().await {
            match command {
                PersistenceCommand::PersistSession {
                    snapshot,
                    respond_to,
                } => {
                    self.handle_persist_session(snapshot, respond_to);
                }
                PersistenceCommand::SearchHistory { query, respond_to } => {
                    let sqlite = self.sqlite.clone();
                    tokio::spawn(async move {
                        let result = run_blocking(move || sqlite.search_sessions(&query)).await;
                        let _ = respond_to.send(result);
                    });
                }
                PersistenceCommand::UpdateAccuracy { update, respond_to } => {
                    let sqlite = self.sqlite.clone();
                    tokio::spawn(async move {
                        let session_id = update.session_id.clone();
                        let flag = update.flag.clone();
                        let remarks = update.remarks.clone();
                        let update_for_blocking = update.clone();
                        let telemetry_session = session_id.clone();
                        let telemetry_flag = flag.clone();
                        let telemetry_remarks = remarks.clone();

                        let result = run_blocking(move || {
                            sqlite.update_accuracy(&update_for_blocking)?;
                            sqlite.enqueue_telemetry(
                                &telemetry_session,
                                "history_accuracy_marked",
                                json!({
                                    "flag": telemetry_flag.as_str(),
                                    "remarks": telemetry_remarks,
                                }),
                            )?;
                            Ok(())
                        })
                        .await;
                        if let Ok(()) = &result {
                            record_session_history_accuracy(
                                &session_id,
                                flag.as_str(),
                                remarks.as_deref(),
                            );
                        }
                        let _ = respond_to.send(result);
                    });
                }
                PersistenceCommand::AppendPostAction {
                    session_id,
                    action,
                    respond_to,
                } => {
                    let sqlite = self.sqlite.clone();
                    tokio::spawn(async move {
                        let kind = action.kind.clone();
                        let session_id_for_blocking = session_id.clone();
                        let action_for_blocking = action.clone();
                        let result = run_blocking(move || {
                            sqlite
                                .append_post_action(&session_id_for_blocking, &action_for_blocking)
                        })
                        .await;
                        if let Ok(_) = &result {
                            record_session_history_action(&session_id, kind.as_str());
                        }
                        let _ = respond_to.send(result);
                    });
                }
                PersistenceCommand::CleanupExpired { now_ms, respond_to } => {
                    let sqlite = self.sqlite.clone();
                    tokio::spawn(async move {
                        let started = Instant::now();
                        let result = run_blocking(move || sqlite.cleanup_expired(now_ms)).await;
                        if let Ok(count) = &result {
                            record_session_history_cleanup(*count, started.elapsed());
                        }
                        let _ = respond_to.send(result);
                    });
                }
                PersistenceCommand::EnqueueTelemetry {
                    session_id,
                    event_type,
                    payload,
                } => {
                    if let Err(err) =
                        self.sqlite
                            .enqueue_telemetry(&session_id, &event_type, payload)
                    {
                        warn!(
                            target: "persistence",
                            session_id,
                            event_type,
                            %err,
                            "failed to enqueue telemetry"
                        );
                    }
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

    fn handle_persist_session(
        &self,
        snapshot: SessionSnapshot,
        respond_to: oneshot::Sender<Result<()>>,
    ) {
        let sqlite = self.sqlite.clone();
        tokio::spawn(async move {
            let mut attempt: u8 = 0;
            let started = Instant::now();
            let mut last_error: Option<anyhow::Error> = None;

            while attempt < PERSISTENCE_RETRIES {
                attempt += 1;
                let snapshot_clone = snapshot.clone();
                let sqlite_clone = sqlite.clone();
                let insert = run_blocking(move || sqlite_clone.insert_session(&snapshot_clone));
                match timeout(Duration::from_millis(PERSISTENCE_TIMEOUT_MS), insert).await {
                    Ok(Ok(())) => {
                        record_session_history_persisted(
                            &snapshot.session_id,
                            attempt,
                            started.elapsed(),
                        );
                        let _ = respond_to.send(Ok(()));
                        return;
                    }
                    Ok(Err(err)) => {
                        warn!(
                            target: "persistence",
                            session_id = %snapshot.session_id,
                            attempt,
                            %err,
                            "session persistence failed"
                        );
                        last_error = Some(err);
                    }
                    Err(_) => {
                        let err = anyhow!("persistence exceeded {}ms", PERSISTENCE_TIMEOUT_MS);
                        warn!(
                            target: "persistence",
                            session_id = %snapshot.session_id,
                            attempt,
                            "session persistence timed out"
                        );
                        last_error = Some(err);
                    }
                }

                sleep(Duration::from_millis(50)).await;
            }

            let error = last_error.unwrap_or_else(|| anyhow!("session persistence failed"));
            record_session_history_persist_failure(
                &snapshot.session_id,
                PERSISTENCE_RETRIES,
                &error,
            );
            let _ = respond_to.send(Err(error));
        });
    }

    fn store_draft(&mut self, record: DraftRecord) -> Result<DraftRecord> {
        info!(
            target: "persistence",
            draft_id = %record.draft_id,
            session_id = %record.session_id,
            "persisting transcript draft"
        );
        Self::push_with_limit(&mut self.drafts, record.clone(), MAX_DRAFT_HISTORY);
        Ok(record)
    }

    fn store_notice(&mut self, record: NoticeRecord) -> Result<NoticeRecord> {
        info!(
            target: "persistence",
            notice_id = %record.notice_id,
            session_id = %record.session_id,
            action = %record.action,
            result = %record.result,
            "persisting publish notice"
        );
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

async fn run_blocking<T, F>(job: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T> + Send + 'static,
{
    tokio::task::spawn_blocking(job)
        .await
        .map_err(|err| anyhow!("blocking task join error: {err}"))?
}

#[cfg(test)]
mod legacy_tests {
    use super::*;
    use tokio::sync::mpsc;

    #[tokio::test]
    async fn saves_draft_with_defaults_and_retrieves_history() {
        let (tx, rx) = mpsc::channel(4);
        let sqlite = Arc::new(SqlitePersistence::bootstrap(SqliteConfig::memory()).unwrap());
        let handle = PersistenceHandle::new(tx.clone(), sqlite.clone());
        tokio::spawn(PersistenceActor::new(sqlite, rx).run());

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
        let sqlite = Arc::new(SqlitePersistence::bootstrap(SqliteConfig::memory()).unwrap());
        let handle = PersistenceHandle::new(tx.clone(), sqlite.clone());
        tokio::spawn(PersistenceActor::new(sqlite, rx).run());

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
        let sqlite = Arc::new(SqlitePersistence::bootstrap(SqliteConfig::memory()).unwrap());
        let handle = PersistenceHandle::new(tx.clone(), sqlite.clone());
        tokio::spawn(PersistenceActor::new(sqlite, rx).run());

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
        assert_eq!(history.len(), 50);:
        assert_eq!(history.first().unwrap().notice_id, "notice-195");
        assert_eq!(
            history.last().unwrap().notice_id,
            format!("notice-{}", MAX_NOTICE_HISTORY + 4)
        );
    }
}
