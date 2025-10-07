//! 观测性初始化脚手架。

pub mod events;

use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime};

use tracing_appender::non_blocking::{NonBlocking, WorkerGuard};
use tracing_subscriber::{fmt, layer::SubscriberExt, EnvFilter, Registry};

const LOG_DIR: &str = "logs/telemetry";
const LOG_DIR_ENV: &str = "FLOWWISPER_TELEMETRY_DIR";
const TELEMETRY_PREFIX: &str = "dual-view.json";
const RETENTION_DAYS: u64 = 7;

static TELEMETRY_GUARD: OnceLock<WorkerGuard> = OnceLock::new();
static TRACING_INIT: OnceLock<()> = OnceLock::new();

pub fn init_tracing() {
    TRACING_INIT.get_or_init(|| {
        let env_filter =
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

        match build_file_writer() {
            Ok((writer, guard)) => {
                let _ = TELEMETRY_GUARD.set(guard);
                let file_layer = fmt::layer()
                    .json()
                    .with_ansi(false)
                    .with_target(true)
                    .with_writer(writer);
                let subscriber = Registry::default()
                    .with(env_filter.clone())
                    .with(fmt::layer().with_target(false))
                    .with(file_layer);

                tracing::subscriber::set_global_default(subscriber)
                    .expect("failed to set global subscriber");
            }
            Err(err) => {
                eprintln!("failed to initialize telemetry file logging: {err}");
                let subscriber = Registry::default()
                    .with(env_filter)
                    .with(fmt::layer().with_target(false));

                tracing::subscriber::set_global_default(subscriber)
                    .expect("failed to set global subscriber");
            }
        }
    });
}

fn build_file_writer() -> io::Result<(NonBlocking, WorkerGuard)> {
    let log_dir = telemetry_dir();
    fs::create_dir_all(&log_dir)?;

    if let Err(err) = prune_old_logs(&log_dir, RETENTION_DAYS) {
        eprintln!("failed to prune telemetry logs: {err}");
    }

    let appender = tracing_appender::rolling::daily(log_dir, TELEMETRY_PREFIX);
    Ok(tracing_appender::non_blocking(appender))
}

fn telemetry_dir() -> PathBuf {
    env::var(LOG_DIR_ENV)
        .ok()
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(LOG_DIR))
}

pub fn flush_tracing() {
    if TELEMETRY_GUARD.get().is_some() {
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[cfg(test)]
mod tests {
    use super::events::{
        record_dual_view_latency, record_dual_view_revert, DualViewSelectionLog, EVENT_LATENCY,
        EVENT_REVERT, TARGET,
    };
    use super::*;
    use serde_json::Value;

    #[test]
    fn telemetry_logs_are_json_enveloped() {
        let temp_dir = tempfile::tempdir().expect("temp telemetry dir");
        env::set_var(LOG_DIR_ENV, temp_dir.path());

        init_tracing();

        record_dual_view_latency(
            7,
            "polished",
            "local",
            true,
            Duration::from_millis(1800),
            true,
        );
        record_dual_view_revert(
            vec![DualViewSelectionLog {
                sentence_id: 7,
                variant: "raw",
            }],
            vec![DualViewSelectionLog {
                sentence_id: 7,
                variant: "raw",
            }],
        );

        flush_tracing();

        let mut attempts = 0;
        let log_path = loop {
            let mut log_files: Vec<_> = fs::read_dir(temp_dir.path())
                .expect("telemetry directory listing")
                .filter_map(|entry| entry.ok())
                .map(|entry| entry.path())
                .collect();

            if let Some(path) = log_files.pop() {
                break path;
            }

            attempts += 1;
            assert!(attempts < 10, "expected telemetry log file to be created");
            std::thread::sleep(Duration::from_millis(50));
        };

        let contents = fs::read_to_string(&log_path).expect("log contents readable");

        let mut saw_latency = false;
        let mut saw_revert = false;

        for line in contents.lines().filter(|line| !line.trim().is_empty()) {
            let record: Value = serde_json::from_str(line).expect("valid telemetry json line");
            let target = record
                .get("target")
                .and_then(|value| value.as_str())
                .unwrap_or_default();
            if target != TARGET {
                continue;
            }

            let fields = record
                .get("fields")
                .and_then(|value| value.as_object())
                .expect("fields object present");
            let event = fields
                .get("event")
                .and_then(|value| value.as_str())
                .unwrap_or_default();

            match event {
                EVENT_LATENCY => {
                    assert_eq!(fields.get("sentence_id").and_then(|v| v.as_u64()), Some(7));
                    assert_eq!(
                        fields.get("variant").and_then(|v| v.as_str()),
                        Some("polished")
                    );
                    assert_eq!(fields.get("source").and_then(|v| v.as_str()), Some("local"));
                    assert_eq!(
                        fields.get("is_primary").and_then(|v| v.as_bool()),
                        Some(true)
                    );
                    assert_eq!(
                        fields.get("latency_ms").and_then(|v| v.as_u64()),
                        Some(1800)
                    );
                    assert_eq!(
                        fields.get("within_sla").and_then(|v| v.as_bool()),
                        Some(true)
                    );

                    let payload = fields
                        .get("payload")
                        .and_then(|value| value.as_str())
                        .expect("latency payload string");
                    let payload_json: Value =
                        serde_json::from_str(payload).expect("latency payload json");
                    assert_eq!(payload_json["sentence_id"], 7);
                    saw_latency = true;
                }
                EVENT_REVERT => {
                    assert_eq!(
                        fields.get("requested_count").and_then(|v| v.as_u64()),
                        Some(1)
                    );
                    assert_eq!(
                        fields.get("applied_count").and_then(|v| v.as_u64()),
                        Some(1)
                    );
                    let payload = fields
                        .get("payload")
                        .and_then(|value| value.as_str())
                        .expect("revert payload string");
                    let payload_json: Value =
                        serde_json::from_str(payload).expect("revert payload json");
                    assert_eq!(
                        payload_json["requested"].as_array().map(|arr| arr.len()),
                        Some(1)
                    );
                    assert_eq!(
                        payload_json["applied"].as_array().map(|arr| arr.len()),
                        Some(1)
                    );
                    saw_revert = true;
                }
                other => panic!("unexpected telemetry event: {other}"),
            }
        }

        assert!(saw_latency, "missing latency telemetry record");
        assert!(saw_revert, "missing revert telemetry record");
    }
}

fn prune_old_logs(log_dir: &Path, retention_days: u64) -> io::Result<()> {
    let retention = Duration::from_secs(retention_days.saturating_mul(24 * 60 * 60));
    let threshold = SystemTime::now()
        .checked_sub(retention)
        .unwrap_or(SystemTime::UNIX_EPOCH);

    for entry in fs::read_dir(log_dir)? {
        let entry = entry?;
        let file_name = match entry.file_name().into_string() {
            Ok(name) => name,
            Err(_) => continue,
        };

        if !file_name.starts_with(TELEMETRY_PREFIX) {
            continue;
        }

        let metadata = match entry.metadata() {
            Ok(metadata) => metadata,
            Err(_) => continue,
        };

        if !metadata.is_file() {
            continue;
        }

        let modified = match metadata.modified() {
            Ok(modified) => modified,
            Err(_) => continue,
        };

        if modified < threshold {
            let _ = fs::remove_file(entry.path());
        }
    }

    Ok(())
}
