use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use r2d2::{Pool, PooledConnection};
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::types::Value;
use rusqlite::{params, Connection, OpenFlags, OptionalExtension, Row};
use serde_json::Value as JsonValue;

use crate::session::history::{
    AccuracyFlag, AccuracyUpdate, HistoryEntry, HistoryPage, HistoryPostAction, HistoryQuery,
    SessionSnapshot, HISTORY_PREVIEW_LIMIT,
};

/// Provides SQLCipher key material for the local database.
pub trait KeyResolver: Send + Sync {
    fn resolve_key(&self) -> Result<Option<String>>;
}

/// Key resolver that reads the key material from the `FLOWWISPER_SQLCIPHER_KEY` env variable.
#[derive(Default)]
pub struct EnvKeyResolver;

impl KeyResolver for EnvKeyResolver {
    fn resolve_key(&self) -> Result<Option<String>> {
        Ok(std::env::var("FLOWWISPER_SQLCIPHER_KEY").ok())
    }
}

/// Storage location configuration for the SQLCipher database.
#[derive(Debug, Clone)]
pub enum SqlitePath {
    File(PathBuf),
    Memory,
}

impl SqlitePath {
    fn to_manager(&self) -> SqliteConnectionManager {
        match self {
            SqlitePath::File(path) => {
                SqliteConnectionManager::file(path).with_flags(Self::open_flags())
            }
            SqlitePath::Memory => SqliteConnectionManager::memory().with_flags(Self::open_flags()),
        }
    }

    fn open_flags() -> OpenFlags {
        OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_FULL_MUTEX
    }

    fn as_path(&self) -> Option<&Path> {
        match self {
            SqlitePath::File(path) => Some(path.as_path()),
            SqlitePath::Memory => None,
        }
    }
}

/// Configuration required to bootstrap SQLCipher persistence.
#[derive(Clone)]
pub struct SqliteConfig {
    pub path: SqlitePath,
    pub pool_size: u32,
    pub busy_timeout: Duration,
    pub key_resolver: Arc<dyn KeyResolver>,
}

impl SqliteConfig {
    pub fn memory() -> Self {
        Self {
            path: SqlitePath::Memory,
            pool_size: 4,
            busy_timeout: Duration::from_millis(250),
            key_resolver: Arc::new(EnvKeyResolver::default()),
        }
    }
}

/// Handle that manages SQLCipher backed persistence.
#[derive(Clone)]
pub struct SqlitePersistence {
    pool: Pool<SqliteConnectionManager>,
    db_path: Option<PathBuf>,
}

impl SqlitePersistence {
    /// Bootstraps a SQLCipher connection pool and runs the database migrations.
    pub fn bootstrap(config: SqliteConfig) -> Result<Self> {
        let key_material = config.key_resolver.resolve_key()?;
        let key_for_init = key_material.clone();
        let busy_timeout = config.busy_timeout;
        let manager = config.path.to_manager().with_init(move |conn| {
            Self::configure_connection(conn, busy_timeout, key_for_init.as_deref())
        });

        let pool = Pool::builder()
            .max_size(config.pool_size)
            .connection_timeout(Duration::from_secs(5))
            .build(manager)
            .context("failed to create SQLCipher connection pool")?;

        {
            let mut conn = pool
                .get()
                .context("failed to acquire SQLCipher bootstrap connection")?;
            Self::verify_encryption(&mut conn, key_material.as_deref())?;
            Self::run_migrations(&mut conn)?;
        }

        Ok(Self {
            pool,
            db_path: config.path.as_path().map(Path::to_path_buf),
        })
    }

    /// Provides access to a pooled connection for custom commands.
    pub fn connection(&self) -> Result<PooledConnection<SqliteConnectionManager>> {
        self.pool
            .get()
            .map_err(|err| anyhow!("failed to obtain SQLCipher connection: {err}"))
    }

    fn configure_connection(
        conn: &mut Connection,
        busy_timeout: Duration,
        key: Option<&str>,
    ) -> rusqlite::Result<()> {
        conn.busy_timeout(busy_timeout)?;
        conn.execute_batch("PRAGMA journal_mode=WAL;")?;
        conn.execute_batch("PRAGMA foreign_keys=ON;")?;
        conn.execute_batch("PRAGMA synchronous=NORMAL;")?;
        if let Some(value) = key {
            conn.pragma_update(None, "key", value)?;
        }
        Ok(())
    }

    fn verify_encryption(conn: &mut Connection, key: Option<&str>) -> Result<()> {
        if key.is_none() {
            // Without key material the database falls back to plaintext mode which is acceptable
            // for integration tests.
            return Ok(());
        }

        let cipher_version: String = conn
            .pragma_query_value(None, "cipher_version", |row| row.get(0))
            .context("cipher_version pragma unsupported; SQLCipher missing")?;

        if cipher_version.trim().is_empty() {
            return Err(anyhow!("SQLCipher cipher_version returned empty value"));
        }
        Ok(())
    }

    fn run_migrations(conn: &mut Connection) -> Result<()> {
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS sessions (
                session_id TEXT PRIMARY KEY,
                started_at_ms INTEGER NOT NULL,
                completed_at_ms INTEGER NOT NULL,
                duration_ms INTEGER NOT NULL,
                locale TEXT,
                app_identifier TEXT,
                app_version TEXT,
                raw_transcript TEXT NOT NULL,
                polished_transcript TEXT NOT NULL,
                confidence_score REAL,
                accuracy_flag TEXT,
                accuracy_remarks TEXT,
                post_actions TEXT NOT NULL DEFAULT '[]',
                expires_at_ms INTEGER NOT NULL,
                metadata TEXT NOT NULL DEFAULT '{}'
            );

            CREATE TABLE IF NOT EXISTS telemetry_queue (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id TEXT NOT NULL,
                event_type TEXT NOT NULL,
                payload TEXT NOT NULL,
                created_at_ms INTEGER NOT NULL,
                delivered INTEGER NOT NULL DEFAULT 0
            );

            CREATE VIRTUAL TABLE IF NOT EXISTS session_index USING fts5(
                session_id UNINDEXED,
                raw_transcript,
                polished_transcript,
                app_identifier,
                content='sessions',
                content_rowid='rowid',
                tokenize='unicode61 remove_diacritics 2'
            );

            CREATE TRIGGER IF NOT EXISTS sessions_ai AFTER INSERT ON sessions BEGIN
                INSERT INTO session_index(rowid, session_id, raw_transcript, polished_transcript, app_identifier)
                VALUES (new.rowid, new.session_id, new.raw_transcript, new.polished_transcript, new.app_identifier);
            END;

            CREATE TRIGGER IF NOT EXISTS sessions_ad AFTER DELETE ON sessions BEGIN
                INSERT INTO session_index(session_index, rowid, session_id, raw_transcript, polished_transcript, app_identifier)
                VALUES('delete', old.rowid, old.session_id, old.raw_transcript, old.polished_transcript, old.app_identifier);
            END;

            CREATE TRIGGER IF NOT EXISTS sessions_au AFTER UPDATE ON sessions BEGIN
                INSERT INTO session_index(session_index, rowid, session_id, raw_transcript, polished_transcript, app_identifier)
                VALUES('delete', old.rowid, old.session_id, old.raw_transcript, old.polished_transcript, old.app_identifier);
                INSERT INTO session_index(rowid, session_id, raw_transcript, polished_transcript, app_identifier)
                VALUES (new.rowid, new.session_id, new.raw_transcript, new.polished_transcript, new.app_identifier);
            END;
            "#,
        )
        .context("failed to run SQLCipher migrations")?;

        // Verify that FTS5 is operational.
        conn.prepare("SELECT count(*) FROM session_index")
            .context("FTS5 session_index missing after migration")?
            .query_row([], |row| row.get::<_, i64>(0))
            .context("failed to read session_index after migration")?;

        Ok(())
    }

    pub fn insert_session(&self, snapshot: &SessionSnapshot) -> Result<()> {
        let mut conn = self.connection()?;
        let tx = conn
            .transaction()
            .context("failed to open transaction for session insert")?;

        let post_actions = serde_json::to_string(&snapshot.post_actions)
            .context("failed to serialize post actions")?;
        let metadata = if snapshot.metadata.is_null() {
            "{}".to_string()
        } else {
            serde_json::to_string(&snapshot.metadata)
                .context("failed to serialize session metadata")?
        };

        tx.execute(
            "INSERT INTO sessions (
                session_id,
                started_at_ms,
                completed_at_ms,
                duration_ms,
                locale,
                app_identifier,
                app_version,
                raw_transcript,
                polished_transcript,
                confidence_score,
                accuracy_flag,
                accuracy_remarks,
                post_actions,
                expires_at_ms,
                metadata
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
            ON CONFLICT(session_id) DO UPDATE SET
                started_at_ms=excluded.started_at_ms,
                completed_at_ms=excluded.completed_at_ms,
                duration_ms=excluded.duration_ms,
                locale=excluded.locale,
                app_identifier=excluded.app_identifier,
                app_version=excluded.app_version,
                raw_transcript=excluded.raw_transcript,
                polished_transcript=excluded.polished_transcript,
                confidence_score=excluded.confidence_score,
                post_actions=excluded.post_actions,
                expires_at_ms=excluded.expires_at_ms,
                metadata=excluded.metadata,
                accuracy_flag=COALESCE(sessions.accuracy_flag, excluded.accuracy_flag),
                accuracy_remarks=COALESCE(sessions.accuracy_remarks, excluded.accuracy_remarks)
            ",
            params![
                snapshot.session_id,
                snapshot.started_at_ms,
                snapshot.completed_at_ms,
                snapshot.duration_ms(),
                snapshot.locale.as_deref(),
                snapshot.app_identifier.as_deref(),
                snapshot.app_version.as_deref(),
                snapshot.raw_transcript,
                snapshot.polished_transcript,
                snapshot.confidence_score,
                AccuracyFlag::Unknown.as_str(),
                Option::<String>::None,
                post_actions,
                snapshot.expires_at_ms(),
                metadata,
            ],
        )
        .context("failed to insert session record")?;

        tx.commit().context("failed to commit session insert")?;
        Ok(())
    }

    pub fn load_session(&self, session_id: &str) -> Result<Option<HistoryEntry>> {
        let conn = self.connection()?;
        let mut stmt = conn.prepare(
            "SELECT session_id, started_at_ms, completed_at_ms, duration_ms, locale,
                app_identifier, app_version, raw_transcript, polished_transcript,
                confidence_score, accuracy_flag, accuracy_remarks, post_actions, metadata
            FROM sessions WHERE session_id = ?1",
        )?;

        let entry = stmt
            .query_row(params![session_id], |row| Self::read_history_entry(row))
            .optional()?;
        Ok(entry)
    }

    pub fn search_sessions(&self, query: &HistoryQuery) -> Result<HistoryPage> {
        let conn = self.connection()?;
        let mut filters = Vec::new();
        let mut values: Vec<Value> = Vec::new();

        if let Some(keyword) = query
            .keyword
            .as_ref()
            .and_then(|value| Some(value.trim().to_string()))
            .filter(|value| !value.is_empty())
        {
            filters.push(
                "rowid IN (SELECT rowid FROM session_index WHERE session_index MATCH ?)"
                    .to_string(),
            );
            values.push(Value::Text(format!("{}*", keyword)));
        }

        if let Some(locale) = query
            .locale
            .as_ref()
            .and_then(|value| Some(value.trim().to_string()))
            .filter(|value| !value.is_empty())
        {
            filters.push("locale = ?".to_string());
            values.push(Value::Text(locale));
        }

        if let Some(app) = query
            .app_identifier
            .as_ref()
            .and_then(|value| Some(value.trim().to_string()))
            .filter(|value| !value.is_empty())
        {
            filters.push("app_identifier = ?".to_string());
            values.push(Value::Text(app));
        }

        let mut base_query = "SELECT session_id, started_at_ms, completed_at_ms, duration_ms, \
            locale, app_identifier, app_version, raw_transcript, polished_transcript, \
            confidence_score, accuracy_flag, accuracy_remarks, post_actions, metadata \
            FROM sessions"
            .to_string();

        if !filters.is_empty() {
            base_query.push_str(" WHERE ");
            base_query.push_str(&filters.join(" AND "));
        }

        base_query.push_str(" ORDER BY completed_at_ms DESC LIMIT ? OFFSET ?");

        let mut page_values = values.clone();
        page_values.push(Value::Integer(query.limit as i64));
        page_values.push(Value::Integer(query.offset as i64));

        let mut stmt = conn.prepare(&base_query)?;
        let mut rows = stmt.query(rusqlite::params_from_iter(page_values.iter()))?;
        let mut entries = Vec::new();
        while let Some(row) = rows.next()? {
            entries.push(Self::read_history_entry(row)?);
        }

        let mut count_sql = "SELECT COUNT(*) FROM sessions".to_string();
        if !filters.is_empty() {
            count_sql.push_str(" WHERE ");
            count_sql.push_str(&filters.join(" AND "));
        }

        let total: i64 = conn
            .prepare(&count_sql)?
            .query_row(rusqlite::params_from_iter(values.iter()), |row| row.get(0))?;

        let next_offset = if (query.offset + entries.len()) < total as usize {
            Some(query.offset + entries.len())
        } else {
            None
        };

        Ok(HistoryPage {
            total: Some(total),
            next_offset,
            entries,
        })
    }

    pub fn update_accuracy(&self, update: &AccuracyUpdate) -> Result<()> {
        let mut conn = self.connection()?;
        let tx = conn
            .transaction()
            .context("failed to open transaction for accuracy update")?;

        let affected = tx.execute(
            "UPDATE sessions SET accuracy_flag = ?2, accuracy_remarks = ?3 WHERE session_id = ?1",
            params![update.session_id, update.flag.as_str(), update.remarks],
        )?;

        if affected == 0 {
            return Err(anyhow!("session {} not found", update.session_id));
        }

        tx.commit()
            .context("failed to commit accuracy update transaction")?;
        Ok(())
    }

    pub fn append_post_action(
        &self,
        session_id: &str,
        action: &HistoryPostAction,
    ) -> Result<Vec<HistoryPostAction>> {
        let mut conn = self.connection()?;
        let tx = conn
            .transaction()
            .context("failed to open transaction for post action")?;

        let existing: Option<String> = tx
            .query_row(
                "SELECT post_actions FROM sessions WHERE session_id = ?1",
                params![session_id],
                |row| row.get(0),
            )
            .optional()?;

        let mut actions = existing
            .as_deref()
            .and_then(|json| serde_json::from_str::<Vec<HistoryPostAction>>(json).ok())
            .unwrap_or_default();
        actions.push(action.clone());
        let encoded = serde_json::to_string(&actions).context("failed to encode post actions")?;

        let updated = tx.execute(
            "UPDATE sessions SET post_actions = ?2 WHERE session_id = ?1",
            params![session_id, encoded],
        )?;

        if updated == 0 {
            return Err(anyhow!("session {session_id} not found for post action"));
        }

        tx.commit()
            .context("failed to commit post action transaction")?;
        Ok(actions)
    }

    pub fn enqueue_telemetry(
        &self,
        session_id: &str,
        event_type: &str,
        payload: JsonValue,
    ) -> Result<()> {
        let conn = self.connection()?;
        let encoded = serde_json::to_string(&payload)
            .context("failed to encode telemetry payload for queue")?;
        conn.execute(
            "INSERT INTO telemetry_queue(session_id, event_type, payload, created_at_ms)
             VALUES (?1, ?2, ?3, strftime('%s','now') * 1000)",
            params![session_id, event_type, encoded],
        )?;
        Ok(())
    }

    fn read_history_entry(row: &Row) -> rusqlite::Result<HistoryEntry> {
        let raw_transcript: String = row.get("raw_transcript")?;
        let polished_transcript: String = row.get("polished_transcript")?;
        let preview_source = if polished_transcript.trim().is_empty() {
            &raw_transcript
        } else {
            &polished_transcript
        };

        let mut preview = preview_source.trim().to_string();
        if preview.len() > HISTORY_PREVIEW_LIMIT {
            preview.truncate(HISTORY_PREVIEW_LIMIT);
            preview.push('…');
        }

        let accuracy_flag =
            AccuracyFlag::from_db(row.get::<_, Option<String>>("accuracy_flag")?.as_deref());
        let accuracy_remarks: Option<String> = row.get("accuracy_remarks")?;

        let post_actions: Vec<HistoryPostAction> = row
            .get::<_, Option<String>>("post_actions")?
            .and_then(|json| serde_json::from_str(&json).ok())
            .unwrap_or_default();

        let metadata = row
            .get::<_, Option<String>>("metadata")?
            .and_then(|json| serde_json::from_str(&json).ok())
            .unwrap_or_else(|| JsonValue::default());

        let confidence_score = row
            .get::<_, Option<f64>>("confidence_score")?
            .map(|value| value as f32);

        Ok(HistoryEntry {
            session_id: row.get("session_id")?,
            started_at_ms: row.get("started_at_ms")?,
            completed_at_ms: row.get("completed_at_ms")?,
            duration_ms: row.get("duration_ms")?,
            locale: row.get("locale")?,
            app_identifier: row.get("app_identifier")?,
            app_version: row.get("app_version")?,
            raw_transcript,
            polished_transcript,
            preview,
            accuracy_flag,
            accuracy_remarks,
            post_actions,
            metadata,
            confidence_score,
        })
    }

    /// Deletes expired sessions according to the configured TTL.
    pub fn cleanup_expired(&self, now_ms: i64) -> Result<usize> {
        let conn = self.connection()?;
        let affected = conn.execute(
            "DELETE FROM sessions WHERE expires_at_ms <= ?1",
            params![now_ms],
        )?;
        Ok(affected)
    }

    pub fn database_path(&self) -> Option<&Path> {
        self.db_path.as_deref()
    }
}

#[cfg(test)]
impl SqlitePersistence {
    pub fn run_migrations_for_tests(conn: &mut Connection) -> Result<()> {
        Self::run_migrations(conn)
    }
}
