use std::env;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration as StdDuration, SystemTime, UNIX_EPOCH};

use dirs::data_dir;
use flowwisper_core::persistence::sqlite::{
    EnvKeyResolver, SqliteConfig, SqlitePath, SqlitePersistence,
};
use flowwisper_core::session::history::{
    AccuracyUpdate, HistoryActionKind, HistoryEntry, HistoryPage, HistoryPostAction, HistoryQuery,
};
use once_cell::sync::OnceCell;
use serde::Deserialize;
use serde_json::Value;
use tauri::async_runtime;

static SQLITE: OnceCell<Arc<SqlitePersistence>> = OnceCell::new();

fn resolve_config() -> Result<SqliteConfig, String> {
    let base_dir = env::var("FLOWWISPER_DATA_DIR")
        .map(PathBuf::from)
        .or_else(|_| data_dir().map(|dir| dir.join("Flowwisper")))
        .ok_or_else(|| "无法定位历史数据库目录".to_string())?;

    fs::create_dir_all(&base_dir).map_err(|err| format!("无法创建数据目录 {base_dir:?}: {err}"))?;

    let db_path = base_dir.join("history.db");
    Ok(SqliteConfig {
        path: SqlitePath::File(db_path),
        pool_size: 8,
        busy_timeout: StdDuration::from_millis(250),
        key_resolver: Arc::new(EnvKeyResolver::default()),
    })
}

fn sqlite() -> Result<Arc<SqlitePersistence>, String> {
    SQLITE
        .get_or_try_init(|| {
            let config = resolve_config()?;
            SqlitePersistence::bootstrap(config)
                .map(Arc::new)
                .map_err(|err| err.to_string())
        })
        .map(|arc| arc.clone())
}

pub async fn search_history(query: HistoryQuery) -> Result<HistoryPage, String> {
    let sqlite = sqlite()?;
    async_runtime::spawn_blocking(move || sqlite.search_sessions(&query))
        .await
        .map_err(|err| err.to_string())?
        .map_err(|err| err.to_string())
}

pub async fn load_history(session_id: String) -> Result<Option<HistoryEntry>, String> {
    let sqlite = sqlite()?;
    async_runtime::spawn_blocking(move || sqlite.load_session(&session_id))
        .await
        .map_err(|err| err.to_string())?
        .map_err(|err| err.to_string())
}

pub async fn mark_accuracy(update: AccuracyUpdate) -> Result<(), String> {
    let sqlite = sqlite()?;
    async_runtime::spawn_blocking(move || sqlite.update_accuracy(&update))
        .await
        .map_err(|err| err.to_string())?
        .map_err(|err| err.to_string())
}

pub async fn append_action(
    session_id: String,
    kind: HistoryActionKind,
    detail: Option<Value>,
) -> Result<Vec<HistoryPostAction>, String> {
    let sqlite = sqlite()?;
    let timestamp_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0);
    let action = HistoryPostAction {
        kind,
        timestamp_ms,
        detail: detail.unwrap_or_else(|| Value::Object(Default::default())),
    };

    async_runtime::spawn_blocking(move || sqlite.append_post_action(&session_id, &action))
        .await
        .map_err(|err| err.to_string())?
        .map_err(|err| err.to_string())
}

#[derive(Debug, Deserialize)]
pub struct HistoryActionRequest {
    pub session_id: String,
    pub action: HistoryActionKind,
    #[serde(default)]
    pub detail: Option<Value>,
}
