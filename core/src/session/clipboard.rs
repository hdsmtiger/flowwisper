//! 剪贴板降级流程的管理工具。
//!
//! 负责备份当前剪贴板内容、写入润色稿，并在需要时恢复原始内容。

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use thiserror::Error;

/// 剪贴板支持的文本内容，仅处理纯文本格式。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardContents {
    text: String,
}

impl ClipboardContents {
    pub fn new<S: Into<String>>(text: S) -> Self {
        Self { text: text.into() }
    }

    pub fn as_str(&self) -> &str {
        &self.text
    }
}

/// 备份的剪贴板快照。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipboardSnapshot {
    contents: Option<ClipboardContents>,
}

impl ClipboardSnapshot {
    pub fn empty() -> Self {
        Self { contents: None }
    }

    pub fn with_contents(contents: ClipboardContents) -> Self {
        Self {
            contents: Some(contents),
        }
    }

    pub fn contents(&self) -> Option<&ClipboardContents> {
        self.contents.as_ref()
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ClipboardError {
    #[error("clipboard read failed: {message}")]
    ReadFailed { message: String },
    #[error("clipboard write failed: {message}")]
    WriteFailed { message: String },
    #[error("clipboard clear failed: {message}")]
    ClearFailed { message: String },
}

impl ClipboardError {
    pub fn read<S: Into<String>>(message: S) -> Self {
        Self::ReadFailed {
            message: message.into(),
        }
    }

    pub fn write<S: Into<String>>(message: S) -> Self {
        Self::WriteFailed {
            message: message.into(),
        }
    }

    pub fn clear<S: Into<String>>(message: S) -> Self {
        Self::ClearFailed {
            message: message.into(),
        }
    }
}

#[async_trait]
pub trait ClipboardAccess: Send + Sync {
    async fn read_text(&self, timeout: Duration) -> Result<Option<String>, ClipboardError>;

    async fn write_text(&self, contents: &str, timeout: Duration) -> Result<(), ClipboardError>;

    async fn clear(&self, timeout: Duration) -> Result<(), ClipboardError>;
}

/// 管理剪贴板备份、写入与恢复。
#[derive(Clone)]
pub struct ClipboardManager {
    access: Arc<dyn ClipboardAccess>,
}

impl std::fmt::Debug for ClipboardManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClipboardManager").finish_non_exhaustive()
    }
}

impl ClipboardManager {
    pub fn new(access: Arc<dyn ClipboardAccess>) -> Self {
        Self { access }
    }

    pub fn with_system() -> Self {
        Self::new(Arc::new(SystemClipboard::default()))
    }

    pub async fn backup(&self, timeout: Duration) -> Result<ClipboardSnapshot, ClipboardError> {
        match self.access.read_text(timeout).await? {
            Some(contents) => Ok(ClipboardSnapshot::with_contents(ClipboardContents::new(
                contents,
            ))),
            None => Ok(ClipboardSnapshot::empty()),
        }
    }

    pub async fn write_with_backup(
        &self,
        contents: &str,
        timeout: Duration,
    ) -> Result<ClipboardFallback, ClipboardError> {
        let snapshot = self.backup(timeout).await?;
        self.access.write_text(contents, timeout).await?;
        Ok(ClipboardFallback::new(
            self.access.clone(),
            snapshot,
            timeout,
            ClipboardContents::new(contents),
        ))
    }

    pub async fn restore(
        &self,
        snapshot: ClipboardSnapshot,
        timeout: Duration,
    ) -> Result<(), ClipboardError> {
        match snapshot.contents {
            Some(contents) => self.access.write_text(contents.as_str(), timeout).await,
            None => self.access.clear(timeout).await,
        }
    }
}

/// 剪贴板降级流程的控制柄，负责在需要时恢复原始内容。
pub struct ClipboardFallback {
    access: Arc<dyn ClipboardAccess>,
    snapshot: Option<ClipboardSnapshot>,
    timeout: Duration,
    replacement: ClipboardContents,
    restored: bool,
}

impl std::fmt::Debug for ClipboardFallback {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClipboardFallback")
            .field("timeout", &self.timeout)
            .field("restored", &self.restored)
            .finish_non_exhaustive()
    }
}

impl ClipboardFallback {
    fn new(
        access: Arc<dyn ClipboardAccess>,
        snapshot: ClipboardSnapshot,
        timeout: Duration,
        replacement: ClipboardContents,
    ) -> Self {
        Self {
            access,
            snapshot: Some(snapshot),
            timeout,
            replacement,
            restored: false,
        }
    }

    pub fn replacement(&self) -> &ClipboardContents {
        &self.replacement
    }

    pub fn has_backup(&self) -> bool {
        self.snapshot.is_some()
    }

    pub fn snapshot(&self) -> Option<&ClipboardSnapshot> {
        self.snapshot.as_ref()
    }

    pub async fn restore(&mut self) -> Result<(), ClipboardError> {
        if self.restored {
            return Ok(());
        }

        if let Some(snapshot) = &self.snapshot {
            match snapshot.contents() {
                Some(contents) => {
                    self.access
                        .write_text(contents.as_str(), self.timeout)
                        .await?
                }
                None => {
                    self.access.clear(self.timeout).await?;
                }
            }
            self.restored = true;
        }

        Ok(())
    }

    pub async fn restore_once(mut self) -> Result<(), ClipboardError> {
        self.restore().await
    }

    pub fn into_snapshot(mut self) -> Option<ClipboardSnapshot> {
        self.snapshot.take()
    }

    pub fn commit(mut self) {
        self.snapshot.take();
        self.restored = true;
    }
}

#[derive(Default)]
struct SystemClipboard;

#[async_trait]
impl ClipboardAccess for SystemClipboard {
    async fn read_text(&self, _timeout: Duration) -> Result<Option<String>, ClipboardError> {
        // TODO(task 2.4): 读取系统剪贴板。
        Ok(None)
    }

    async fn write_text(&self, _contents: &str, _timeout: Duration) -> Result<(), ClipboardError> {
        // TODO(task 2.4): 写入系统剪贴板。
        Ok(())
    }

    async fn clear(&self, _timeout: Duration) -> Result<(), ClipboardError> {
        // TODO(task 2.4): 清空系统剪贴板。
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::time::Duration;
    use tokio::sync::Mutex;

    #[derive(Clone, Default)]
    struct MockClipboardAccess {
        state: Arc<Mutex<Option<String>>>,
        read_error: Arc<Mutex<Option<ClipboardError>>>,
        write_error: Arc<Mutex<Option<ClipboardError>>>,
        clear_error: Arc<Mutex<Option<ClipboardError>>>,
    }

    #[async_trait]
    impl ClipboardAccess for MockClipboardAccess {
        async fn read_text(&self, _timeout: Duration) -> Result<Option<String>, ClipboardError> {
            if let Some(err) = self.read_error.lock().await.clone() {
                return Err(err);
            }
            Ok(self.state.lock().await.clone())
        }

        async fn write_text(
            &self,
            contents: &str,
            _timeout: Duration,
        ) -> Result<(), ClipboardError> {
            if let Some(err) = self.write_error.lock().await.clone() {
                return Err(err);
            }
            *self.state.lock().await = Some(contents.to_string());
            Ok(())
        }

        async fn clear(&self, _timeout: Duration) -> Result<(), ClipboardError> {
            if let Some(err) = self.clear_error.lock().await.clone() {
                return Err(err);
            }
            *self.state.lock().await = None;
            Ok(())
        }
    }

    impl MockClipboardAccess {
        async fn set_state(&self, value: Option<&str>) {
            *self.state.lock().await = value.map(|s| s.to_string());
        }

        async fn inject_read_error(&self, error: ClipboardError) {
            *self.read_error.lock().await = Some(error);
        }

        async fn inject_write_error(&self, error: ClipboardError) {
            *self.write_error.lock().await = Some(error);
        }

        async fn inject_clear_error(&self, error: ClipboardError) {
            *self.clear_error.lock().await = Some(error);
        }
    }

    fn manager() -> ClipboardManager {
        let access = Arc::new(MockClipboardAccess::default());
        ClipboardManager::new(access)
    }

    #[tokio::test]
    async fn backup_returns_existing_contents() {
        let access = Arc::new(MockClipboardAccess::default());
        access.set_state(Some("existing")).await;
        let manager = ClipboardManager::new(access);

        let snapshot = manager
            .backup(Duration::from_millis(10))
            .await
            .expect("backup should succeed");

        assert_eq!(
            snapshot.contents().map(ClipboardContents::as_str),
            Some("existing")
        );
    }

    #[tokio::test]
    async fn backup_handles_empty_clipboard() {
        let manager = manager();
        let snapshot = manager
            .backup(Duration::from_millis(10))
            .await
            .expect("backup should succeed");

        assert!(snapshot.contents().is_none());
    }

    #[tokio::test]
    async fn write_with_backup_replaces_contents() {
        let access = Arc::new(MockClipboardAccess::default());
        access.set_state(Some("old")).await;
        let manager = ClipboardManager::new(access.clone());

        let mut fallback = manager
            .write_with_backup("new", Duration::from_millis(10))
            .await
            .expect("write should succeed");

        let current = access
            .state
            .lock()
            .await
            .clone()
            .expect("clipboard should hold new value");
        assert_eq!(current, "new");

        fallback.restore().await.expect("restore should succeed");
        let restored = access.state.lock().await.clone();
        assert_eq!(restored, Some("old".to_string()));
    }

    #[tokio::test]
    async fn restore_once_consumes_handle() {
        let access = Arc::new(MockClipboardAccess::default());
        access.set_state(Some("one")).await;
        let manager = ClipboardManager::new(access.clone());

        let fallback = manager
            .write_with_backup("two", Duration::from_millis(10))
            .await
            .expect("write should succeed");

        fallback
            .restore_once()
            .await
            .expect("restore should succeed");

        let restored = access.state.lock().await.clone();
        assert_eq!(restored, Some("one".to_string()));
    }

    #[tokio::test]
    async fn restore_is_idempotent() {
        let access = Arc::new(MockClipboardAccess::default());
        access.set_state(Some("origin")).await;
        let manager = ClipboardManager::new(access.clone());

        let mut fallback = manager
            .write_with_backup("temp", Duration::from_millis(10))
            .await
            .expect("write should succeed");

        fallback.restore().await.expect("first restore succeeds");
        fallback.restore().await.expect("second restore is no-op");
    }

    #[tokio::test]
    async fn restore_empty_snapshot_clears_clipboard() {
        let access = Arc::new(MockClipboardAccess::default());
        access.set_state(Some("existing")).await;
        let manager = ClipboardManager::new(access.clone());

        let snapshot = ClipboardSnapshot::empty();
        manager
            .restore(snapshot.clone(), Duration::from_millis(10))
            .await
            .expect("restore should succeed");

        assert!(access.state.lock().await.is_none());
    }

    #[tokio::test]
    async fn write_with_backup_propagates_errors() {
        let access = Arc::new(MockClipboardAccess::default());
        access.inject_read_error(ClipboardError::read("read")).await;
        let manager = ClipboardManager::new(access);

        let result = manager
            .write_with_backup("value", Duration::from_millis(10))
            .await;

        assert!(matches!(result, Err(ClipboardError::ReadFailed { .. })));
    }

    #[tokio::test]
    async fn restore_propagates_clear_errors() {
        let access = Arc::new(MockClipboardAccess::default());
        access
            .inject_clear_error(ClipboardError::clear("clear"))
            .await;
        let manager = ClipboardManager::new(access.clone());

        let snapshot = ClipboardSnapshot::empty();
        let result = manager.restore(snapshot, Duration::from_millis(10)).await;

        assert!(matches!(result, Err(ClipboardError::ClearFailed { .. })));
    }
}
