use std::sync::Arc;
use std::time::Duration;

use rusqlite::{Connection, OpenFlags};
use tempfile::NamedTempFile;

use super::sqlite::{KeyResolver, SqliteConfig, SqlitePath, SqlitePersistence, MAX_TELEMETRY_QUEUE};
use crate::session::history::{
    AccuracyFlag, AccuracyUpdate, HistoryActionKind, HistoryPostAction, HistoryQuery,
    SessionSnapshot,
};
use serde_json::json;

struct StaticKeyResolver(Option<String>);

impl KeyResolver for StaticKeyResolver {
    fn resolve_key(&self) -> anyhow::Result<Option<String>> {
        Ok(self.0.clone())
    }
}

fn config_with_key(path: SqlitePath, key: Option<&str>) -> SqliteConfig {
    let mut config = SqliteConfig {
        path,
        pool_size: 4,
        busy_timeout: Duration::from_millis(200),
        key_resolver: Arc::new(StaticKeyResolver(key.map(|value| value.to_string()))),
    };
    if key.is_none() {
        config.key_resolver = Arc::new(StaticKeyResolver(None));
    }
    config
}

#[test]
fn bootstrap_runs_migrations() {
    let config = config_with_key(SqlitePath::Memory, Some("memory-secret"));
    let persistence = SqlitePersistence::bootstrap(config).expect("bootstrap should succeed");
    let mut conn = persistence.connection().expect("connection available");

    let tables: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE name = 'sessions'",
            [],
            |row| row.get(0),
        )
        .expect("sessions table should exist");
    assert_eq!(tables, 1);

    let triggers: i64 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE name LIKE 'sessions_%' AND type = 'trigger'",
            [],
            |row| row.get(0),
        )
        .expect("triggers should exist");
    assert!(triggers >= 3);
}

#[test]
fn migrations_are_idempotent() {
    let config = config_with_key(SqlitePath::Memory, Some("idempotent"));
    let persistence = SqlitePersistence::bootstrap(config).expect("bootstrap should succeed");
    {
        let mut conn = persistence.connection().expect("connection available");
        SqlitePersistence::run_migrations_for_tests(&mut conn).expect("rerun migrations");
    }
    {
        let mut conn = persistence.connection().expect("connection available");
        SqlitePersistence::run_migrations_for_tests(&mut conn).expect("third run succeeds");
        let (session_cols, index_cols): (i64, i64) = conn
            .query_row(
                "SELECT \
                    (SELECT count(*) FROM pragma_table_info('sessions')),\
                    (SELECT count(*) FROM pragma_table_info('session_index'))",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("schema introspection");
        assert!(session_cols >= 10, "sessions table should retain columns");
        assert!(index_cols >= 4, "session_index retains columns");
    }
}

#[test]
fn encrypted_database_rejects_wrong_key() {
    let temp = NamedTempFile::new().expect("temp file");
    let config = config_with_key(
        SqlitePath::File(temp.path().to_path_buf()),
        Some("correct-horse-battery-staple"),
    );
    let persistence = SqlitePersistence::bootstrap(config).expect("bootstrap should succeed");
    drop(persistence);

    let mut conn = Connection::open_with_flags(
        temp.path(),
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_FULL_MUTEX,
    )
    .expect("able to open raw connection");
    conn.pragma_update(None, "key", "incorrect-key")
        .expect("able to provide key");

    let err = conn.query_row::<i64, _, _>("SELECT count(*) FROM sessions", [], |row| row.get(0));
    assert!(err.is_err(), "query should fail with wrong key");
}

fn sample_snapshot(id: &str) -> SessionSnapshot {
    SessionSnapshot {
        session_id: id.into(),
        started_at_ms: 1_000,
        completed_at_ms: 2_000,
        locale: Some("en-US".into()),
        app_identifier: Some("com.example.app".into()),
        app_version: Some("1.2.3".into()),
        confidence_score: Some(0.87),
        raw_transcript: "raw history text".into(),
        polished_transcript: "polished history text".into(),
        metadata: json!({"origin": "test"}),
        post_actions: vec![],
    }
}

#[test]
fn insert_session_and_search_returns_entry() {
    let config = SqliteConfig::memory();
    let persistence = SqlitePersistence::bootstrap(config).expect("bootstrap should succeed");
    let snapshot = sample_snapshot("history-1");
    persistence
        .insert_session(&snapshot)
        .expect("insert should succeed");

    let page = persistence
        .search_sessions(&HistoryQuery::default())
        .expect("search succeeds");
    assert_eq!(page.entries.len(), 1);
    let entry = &page.entries[0];
    assert_eq!(entry.session_id, "history-1");
    assert_eq!(entry.locale.as_deref(), Some("en-US"));
    assert!(entry.preview.contains("polished"));
}

#[test]
fn update_accuracy_persists_flags() {
    let config = SqliteConfig::memory();
    let persistence = SqlitePersistence::bootstrap(config).expect("bootstrap should succeed");
    let snapshot = sample_snapshot("history-accuracy");
    persistence
        .insert_session(&snapshot)
        .expect("insert should succeed");

    let update = AccuracyUpdate {
        session_id: "history-accuracy".into(),
        flag: AccuracyFlag::InaccurateRaw,
        remarks: Some("names misheard".into()),
    };
    persistence
        .update_accuracy(&update)
        .expect("update should succeed");

    let entry = persistence
        .load_session("history-accuracy")
        .expect("load query should succeed")
        .expect("history entry present");
    assert!(matches!(entry.accuracy_flag, AccuracyFlag::InaccurateRaw));
    assert_eq!(entry.accuracy_remarks.as_deref(), Some("names misheard"));
}

#[test]
fn append_post_action_records_history() {
    let config = SqliteConfig::memory();
    let persistence = SqlitePersistence::bootstrap(config).expect("bootstrap should succeed");
    let snapshot = sample_snapshot("history-action");
    persistence
        .insert_session(&snapshot)
        .expect("insert should succeed");

    let action = HistoryPostAction {
        kind: HistoryActionKind::Copy,
        timestamp_ms: 9_000,
        detail: json!({"channel": "clipboard"}),
    };

    let actions = persistence
        .append_post_action("history-action", &action)
        .expect("action append succeeds");
    assert_eq!(actions.len(), 1);
    assert_eq!(actions[0].kind, HistoryActionKind::Copy);

    let entry = persistence
        .load_session("history-action")
        .expect("load succeeds")
        .expect("entry exists");
    assert_eq!(entry.post_actions.len(), 1);
}

#[test]
fn enqueue_telemetry_records_event() {
    let config = SqliteConfig::memory();
    let persistence = SqlitePersistence::bootstrap(config).expect("bootstrap should succeed");
    persistence
        .enqueue_telemetry("session-t", "history_accuracy_marked", json!({"flag": "accurate"}))
        .expect("enqueue succeeds");

    let count: i64 = persistence
        .connection()
        .expect("conn")
        .query_row("SELECT count(*) FROM telemetry_queue", [], |row| row.get(0))
        .expect("query");
    assert_eq!(count, 1);
}

#[test]
fn telemetry_queue_prunes_to_capacity() {
    let config = SqliteConfig::memory();
    let persistence = SqlitePersistence::bootstrap(config).expect("bootstrap should succeed");

    for idx in 0..(MAX_TELEMETRY_QUEUE + 75) {
        persistence
            .enqueue_telemetry(
                "session-prune",
                "noise_event",
                json!({"seq": idx}),
            )
            .expect("enqueue telemetry");
    }

    let count: i64 = persistence
        .connection()
        .expect("conn")
        .query_row("SELECT count(*) FROM telemetry_queue", [], |row| row.get(0))
        .expect("query count");

    assert!(count <= MAX_TELEMETRY_QUEUE);
    assert!(count >= 100, "queue should retain at least 100 events");
}

#[test]
fn search_applies_keyword_and_filters() {
    let config = SqliteConfig::memory();
    let persistence = SqlitePersistence::bootstrap(config).expect("bootstrap should succeed");
    let mut first = sample_snapshot("history-filter-1");
    first.polished_transcript = "special keyword transcript".into();
    first.app_identifier = Some("com.example.filtered".into());
    persistence
        .insert_session(&first)
        .expect("insert first");

    let mut second = sample_snapshot("history-filter-2");
    second.polished_transcript = "different text".into();
    second.app_identifier = Some("com.other.app".into());
    persistence
        .insert_session(&second)
        .expect("insert second");

    let query = HistoryQuery {
        keyword: Some("keyword".into()),
        locale: None,
        app_identifier: Some("com.example.filtered".into()),
        limit: 10,
        offset: 0,
    };

    let page = persistence.search_sessions(&query).expect("search succeeds");
    assert_eq!(page.entries.len(), 1);
    assert_eq!(page.entries[0].session_id, "history-filter-1");
}

#[test]
fn search_respects_pagination() {
    let config = SqliteConfig::memory();
    let persistence = SqlitePersistence::bootstrap(config).expect("bootstrap should succeed");
    for idx in 0..3 {
        let mut snapshot = sample_snapshot(&format!("history-page-{idx}"));
        snapshot.completed_at_ms = 1_000 + (idx as i64 * 1_000);
        persistence
            .insert_session(&snapshot)
            .expect("insert snapshot");
    }

    let mut query = HistoryQuery::default();
    query.limit = 1;

    let first_page = persistence.search_sessions(&query).expect("first page");
    assert_eq!(first_page.entries.len(), 1);
    let first_id = &first_page.entries[0].session_id;

    query.offset = first_page.next_offset.unwrap_or(1);
    let second_page = persistence.search_sessions(&query).expect("second page");
    assert_eq!(second_page.entries.len(), 1);
    assert_ne!(second_page.entries[0].session_id, *first_id);
}

#[test]
fn cleanup_expired_removes_sessions() {
    let config = SqliteConfig::memory();
    let persistence = SqlitePersistence::bootstrap(config).expect("bootstrap should succeed");
    let mut expired = sample_snapshot("history-expired");
    expired.completed_at_ms = 0;
    persistence
        .insert_session(&expired)
        .expect("insert expired");

    let removed = persistence
        .cleanup_expired(expired.expires_at_ms() + 1)
        .expect("cleanup succeeds");
    assert_eq!(removed, 1);

    let page = persistence
        .search_sessions(&HistoryQuery::default())
        .expect("search succeeds");
    assert!(page.entries.is_empty());
}
