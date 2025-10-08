//! 会话发布（插入）流程的入口定义。
//!
//! 该模块专注于封装“润色稿 -> 焦点窗口”插入动作的编排，
//! 后续任务会在此基础上实现跨平台可访问性检测、剪贴板降级等细节。

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use thiserror::Error;

/// 描述当前焦点窗口的上下文信息，用于辅助决策插入策略。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FocusWindowContext {
    /// 操作系统级别的应用标识，如 bundle identifier 或进程名。
    pub app_identifier: Option<String>,
    /// 焦点窗口标题，可用于调试或通知中心记录。
    pub window_title: Option<String>,
    /// 补充上下文，例如编辑模式、输入法提示等。
    pub metadata: Option<String>,
}

impl FocusWindowContext {
    /// 便捷构造函数，仅提供应用标识。
    pub fn from_app_identifier<S: Into<String>>(identifier: S) -> Self {
        Self {
            app_identifier: Some(identifier.into()),
            ..Self::default()
        }
    }
}

/// 插入失败后允许的回退策略。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FallbackStrategy {
    /// 不允许自动降级，由上层交互决定后续动作。
    None,
    /// 将润色稿写入剪贴板，提示用户粘贴或撤销。
    ClipboardCopy,
    /// 仅通知用户保留原稿，常用于敏感或不可写的窗口。
    NotifyOnly,
}

impl Default for FallbackStrategy {
    fn default() -> Self {
        Self::ClipboardCopy
    }
}

impl FallbackStrategy {
    pub fn as_str(&self) -> &'static str {
        match self {
            FallbackStrategy::None => "none",
            FallbackStrategy::ClipboardCopy => "clipboard_copy",
            FallbackStrategy::NotifyOnly => "notify_only",
        }
    }
}

/// 执行插入时的配置项。
#[derive(Debug, Clone)]
pub struct PublisherConfig {
    /// 单次直接插入尝试允许的最长时长。
    pub direct_insert_timeout: Duration,
    /// 当触发降级策略时，复制到剪贴板的最长等待时长。
    pub fallback_timeout: Duration,
    /// 允许的最大重试次数（不含首次尝试）。
    pub max_retry: u8,
}

impl Default for PublisherConfig {
    fn default() -> Self {
        Self {
            direct_insert_timeout: Duration::from_millis(400),
            fallback_timeout: Duration::from_millis(200),
            max_retry: 1,
        }
    }
}

/// 触发插入所需的输入。
#[derive(Debug, Clone)]
pub struct PublishRequest {
    /// 最终润色后的文本内容。
    pub transcript: String,
    /// 焦点窗口上下文。
    pub focus: FocusWindowContext,
    /// 失败后的回退策略。
    pub fallback: FallbackStrategy,
}

impl PublishRequest {
    /// 确保文本内容经过裁剪，避免因空白导致误判。
    pub fn validate(&self) -> Result<(), PublisherError> {
        if self.transcript.trim().is_empty() {
            return Err(PublisherError::EmptyTranscript);
        }

        Ok(())
    }
}

/// 插入过程中可能产出的状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublisherStatus {
    /// 插入流程顺利完成。
    Completed,
    /// 插入未执行，等待上层进一步处理。
    Deferred,
    /// 插入失败，且无法通过允许的回退策略恢复。
    Failed,
}

/// 实际采用的执行策略。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublishStrategy {
    /// 直接向焦点窗口插入文本。
    DirectInsert,
    /// 自动执行剪贴板复制，由用户自行粘贴。
    ClipboardFallback,
    /// 仅发出通知或记录草稿，不做插入。
    NotifyOnly,
}

/// 插入失败时的标准化错误码。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublisherFailureCode {
    Timeout,
    PermissionDenied,
    FocusLost,
    ChannelUnavailable,
    AutomationRejected,
    Unknown,
}

impl PublisherFailureCode {
    pub fn as_str(&self) -> &'static str {
        match self {
            PublisherFailureCode::Timeout => "timeout",
            PublisherFailureCode::PermissionDenied => "permission_denied",
            PublisherFailureCode::FocusLost => "focus_lost",
            PublisherFailureCode::ChannelUnavailable => "channel_unavailable",
            PublisherFailureCode::AutomationRejected => "automation_rejected",
            PublisherFailureCode::Unknown => "unknown",
        }
    }
}

/// 插入失败的完整上下文。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublisherFailure {
    pub code: PublisherFailureCode,
    pub message: String,
    pub last_error: Option<AutomationError>,
}

impl PublisherFailure {
    pub fn new<S: Into<String>>(code: PublisherFailureCode, message: S) -> Self {
        Self {
            code,
            message: message.into(),
            last_error: None,
        }
    }

    pub fn with_error<S: Into<String>>(
        code: PublisherFailureCode,
        message: S,
        error: AutomationError,
    ) -> Self {
        Self {
            code,
            message: message.into(),
            last_error: Some(error),
        }
    }

    pub fn from_automation_error(error: AutomationError) -> Self {
        let code = match &error {
            AutomationError::Timeout => PublisherFailureCode::Timeout,
            AutomationError::PermissionDenied => PublisherFailureCode::PermissionDenied,
            AutomationError::FocusNotFound => PublisherFailureCode::FocusLost,
            AutomationError::ChannelUnavailable { .. } => PublisherFailureCode::ChannelUnavailable,
            AutomationError::Other { .. } => PublisherFailureCode::Unknown,
        };

        let message = error.to_string();
        Self::with_error(code, message, error)
    }
}

/// 插入动作的最终产出。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishOutcome {
    /// 本次插入流程的最终状态。
    pub status: PublisherStatus,
    /// 执行过程中采用的策略。
    pub strategy: PublishStrategy,
    /// 实际发生的尝试次数（含初始尝试）。
    pub attempts: u8,
    /// 若触发降级策略，记录具体策略以便遥测与 UI 展示。
    pub fallback: Option<FallbackStrategy>,
    /// 若插入失败，附带失败详情供 UI 展示。
    pub failure: Option<PublisherFailure>,
}

impl PublishOutcome {
    pub fn completed() -> Self {
        Self::completed_with_attempts(PublishStrategy::DirectInsert, 1)
    }

    pub fn completed_with_attempts(strategy: PublishStrategy, attempts: u8) -> Self {
        Self {
            status: PublisherStatus::Completed,
            strategy,
            attempts,
            fallback: None,
            failure: None,
        }
    }

    pub fn deferred(strategy: PublishStrategy, fallback: Option<FallbackStrategy>) -> Self {
        Self {
            status: PublisherStatus::Deferred,
            strategy,
            attempts: 0,
            fallback,
            failure: None,
        }
    }

    pub fn failed(
        attempts: u8,
        strategy: PublishStrategy,
        fallback: Option<FallbackStrategy>,
        failure: PublisherFailure,
    ) -> Self {
        Self {
            status: PublisherStatus::Failed,
            strategy,
            attempts,
            fallback,
            failure: Some(failure),
        }
    }
}

impl PublishStrategy {
    pub fn as_str(&self) -> &'static str {
        match self {
            PublishStrategy::DirectInsert => "direct_insert",
            PublishStrategy::ClipboardFallback => "clipboard_fallback",
            PublishStrategy::NotifyOnly => "notify_only",
        }
    }
}

impl PublisherStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            PublisherStatus::Completed => "completed",
            PublisherStatus::Deferred => "deferred",
            PublisherStatus::Failed => "failed",
        }
    }
}

/// 表示焦点窗口支持的自动化能力。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FocusCapabilities {
    pub is_writable: bool,
    pub supports_clipboard_paste: bool,
    pub supports_keystroke_injection: bool,
    pub reason: Option<String>,
}

impl FocusCapabilities {
    pub fn writable_with_clipboard() -> Self {
        Self {
            is_writable: true,
            supports_clipboard_paste: true,
            supports_keystroke_injection: false,
            reason: None,
        }
    }

    pub fn writable_with_keystroke() -> Self {
        Self {
            is_writable: true,
            supports_clipboard_paste: false,
            supports_keystroke_injection: true,
            reason: None,
        }
    }

    pub fn writable_with_all_channels() -> Self {
        Self {
            is_writable: true,
            supports_clipboard_paste: true,
            supports_keystroke_injection: true,
            reason: None,
        }
    }

    pub fn read_only<S: Into<String>>(reason: S) -> Self {
        Self {
            is_writable: false,
            supports_clipboard_paste: false,
            supports_keystroke_injection: false,
            reason: Some(reason.into()),
        }
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum AutomationError {
    #[error("operation timed out")]
    Timeout,
    #[error("accessibility permission denied")]
    PermissionDenied,
    #[error("focus window not found")]
    FocusNotFound,
    #[error("automation channel unavailable: {message}")]
    ChannelUnavailable { message: String },
    #[error("automation failed: {message}")]
    Other { message: String },
}

impl AutomationError {
    pub fn channel_unavailable<S: Into<String>>(message: S) -> Self {
        Self::ChannelUnavailable {
            message: message.into(),
        }
    }

    pub fn other<S: Into<String>>(message: S) -> Self {
        Self::Other {
            message: message.into(),
        }
    }

    pub fn focus_not_found() -> Self {
        Self::FocusNotFound
    }
}

#[async_trait]
pub trait FocusAutomation: Send + Sync {
    async fn inspect_focus(
        &self,
        context: &FocusWindowContext,
        timeout: Duration,
    ) -> Result<FocusCapabilities, AutomationError>;

    async fn paste_via_clipboard(
        &self,
        contents: &str,
        timeout: Duration,
    ) -> Result<(), AutomationError>;

    async fn simulate_keystrokes(
        &self,
        contents: &str,
        timeout: Duration,
    ) -> Result<(), AutomationError>;
}

/// 发布器负责协调插入与降级的执行。
pub struct Publisher {
    config: PublisherConfig,
    automation: Arc<dyn FocusAutomation>,
}

impl std::fmt::Debug for Publisher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Publisher")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl Clone for Publisher {
    fn clone(&self) -> Self {
        Self {
            config: self.config.clone(),
            automation: self.automation.clone(),
        }
    }
}

impl Publisher {
    pub fn new(config: PublisherConfig, automation: Arc<dyn FocusAutomation>) -> Self {
        Self { config, automation }
    }

    pub fn with_automation(automation: Arc<dyn FocusAutomation>) -> Self {
        Self::new(PublisherConfig::default(), automation)
    }

    pub fn config(&self) -> &PublisherConfig {
        &self.config
    }

    pub fn automation(&self) -> Arc<dyn FocusAutomation> {
        self.automation.clone()
    }

    /// 执行插入流程。
    pub async fn publish(&self, request: PublishRequest) -> Result<PublishOutcome, PublisherError> {
        request.validate()?;

        let max_attempts = self.config.max_retry.saturating_add(1);
        let mut attempts: u8 = 0;
        let mut last_failure: Option<PublisherFailure> = None;

        while attempts < max_attempts {
            attempts = attempts.saturating_add(1);

            let capabilities = match self
                .automation
                .inspect_focus(&request.focus, self.config.direct_insert_timeout)
                .await
            {
                Ok(capabilities) => capabilities,
                Err(error) => {
                    last_failure = Some(PublisherFailure::from_automation_error(error));
                    if attempts >= max_attempts {
                        break;
                    } else {
                        continue;
                    }
                }
            };

            if !capabilities.is_writable {
                let reason = capabilities
                    .reason
                    .unwrap_or_else(|| "focus target rejected automation".to_string());
                let failure =
                    PublisherFailure::new(PublisherFailureCode::AutomationRejected, reason);
                return Ok(PublishOutcome::failed(
                    attempts,
                    PublishStrategy::DirectInsert,
                    None,
                    failure,
                ));
            }

            if !capabilities.supports_clipboard_paste && !capabilities.supports_keystroke_injection
            {
                let reason = capabilities
                    .reason
                    .unwrap_or_else(|| "no automation channel available".to_string());
                let failure =
                    PublisherFailure::new(PublisherFailureCode::ChannelUnavailable, reason);
                return Ok(PublishOutcome::failed(
                    attempts,
                    PublishStrategy::DirectInsert,
                    None,
                    failure,
                ));
            }

            let mut channel_failure: Option<PublisherFailure> = None;

            if capabilities.supports_clipboard_paste {
                match self
                    .automation
                    .paste_via_clipboard(&request.transcript, self.config.direct_insert_timeout)
                    .await
                {
                    Ok(()) => {
                        return Ok(PublishOutcome::completed_with_attempts(
                            PublishStrategy::DirectInsert,
                            attempts,
                        ));
                    }
                    Err(error) => {
                        channel_failure = Some(PublisherFailure::from_automation_error(error));
                    }
                }
            }

            if capabilities.supports_keystroke_injection {
                match self
                    .automation
                    .simulate_keystrokes(&request.transcript, self.config.direct_insert_timeout)
                    .await
                {
                    Ok(()) => {
                        return Ok(PublishOutcome::completed_with_attempts(
                            PublishStrategy::DirectInsert,
                            attempts,
                        ));
                    }
                    Err(error) => {
                        channel_failure = Some(PublisherFailure::from_automation_error(error));
                    }
                }
            }

            if let Some(failure) = channel_failure {
                last_failure = Some(failure);
            }
        }

        let failure = last_failure.unwrap_or_else(|| {
            PublisherFailure::new(
                PublisherFailureCode::Unknown,
                "publisher failed after exhausting retries",
            )
        });

        Ok(PublishOutcome::failed(
            attempts.max(1),
            PublishStrategy::DirectInsert,
            None,
            failure,
        ))
    }
}

impl Default for Publisher {
    fn default() -> Self {
        Self::with_automation(Arc::new(SystemFocusAutomation::default()))
    }
}

#[async_trait]
pub trait SessionPublisher: Send + Sync {
    async fn publish(&self, request: PublishRequest) -> Result<PublishOutcome, PublisherError>;
}

#[async_trait]
impl SessionPublisher for Publisher {
    async fn publish(&self, request: PublishRequest) -> Result<PublishOutcome, PublisherError> {
        Publisher::publish(self, request).await
    }
}

#[derive(Default)]
struct SystemFocusAutomation;

#[async_trait]
impl FocusAutomation for SystemFocusAutomation {
    async fn inspect_focus(
        &self,
        _context: &FocusWindowContext,
        _timeout: Duration,
    ) -> Result<FocusCapabilities, AutomationError> {
        // TODO(task 2.1+): 实现真实的跨平台可写性检测，当前默认允许插入。
        Ok(FocusCapabilities::writable_with_all_channels())
    }

    async fn paste_via_clipboard(
        &self,
        _contents: &str,
        _timeout: Duration,
    ) -> Result<(), AutomationError> {
        // TODO(task 2.1+): 调用系统粘贴操作。
        Ok(())
    }

    async fn simulate_keystrokes(
        &self,
        _contents: &str,
        _timeout: Duration,
    ) -> Result<(), AutomationError> {
        // TODO(task 2.1+): 调用系统键入模拟。
        Ok(())
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PublisherError {
    #[error("transcript cannot be empty")]
    EmptyTranscript,
    #[error("focus window not writable: {reason}")]
    FocusNotWritable { reason: String },
    #[error("no automation channel available: {reason}")]
    AutomationChannelUnavailable { reason: String },
    #[error("focus inspection failed: {0}")]
    FocusInspectionFailed(AutomationError),
    #[error("insertion failed: {0}")]
    InsertionFailed(AutomationError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tokio::sync::Mutex;

    #[derive(Clone)]
    struct MockAutomation {
        inspect_result: Arc<Mutex<Result<FocusCapabilities, AutomationError>>>,
        paste_calls: Arc<Mutex<Vec<String>>>,
        keystroke_calls: Arc<Mutex<Vec<String>>>,
        paste_result: Arc<Mutex<Result<(), AutomationError>>>,
        keystroke_result: Arc<Mutex<Result<(), AutomationError>>>,
    }

    impl MockAutomation {
        fn with_capabilities(capabilities: FocusCapabilities) -> Self {
            Self {
                inspect_result: Arc::new(Mutex::new(Ok(capabilities))),
                paste_calls: Arc::new(Mutex::new(Vec::new())),
                keystroke_calls: Arc::new(Mutex::new(Vec::new())),
                paste_result: Arc::new(Mutex::new(Ok(()))),
                keystroke_result: Arc::new(Mutex::new(Ok(()))),
            }
        }

        fn with_inspect_error(error: AutomationError) -> Self {
            Self {
                inspect_result: Arc::new(Mutex::new(Err(error))),
                paste_calls: Arc::new(Mutex::new(Vec::new())),
                keystroke_calls: Arc::new(Mutex::new(Vec::new())),
                paste_result: Arc::new(Mutex::new(Ok(()))),
                keystroke_result: Arc::new(Mutex::new(Ok(()))),
            }
        }

        async fn set_paste_error(&self, error: AutomationError) {
            let mut lock = self.paste_result.lock().await;
            *lock = Err(error);
        }

        async fn set_keystroke_error(&self, error: AutomationError) {
            let mut lock = self.keystroke_result.lock().await;
            *lock = Err(error);
        }

        async fn paste_calls(&self) -> Vec<String> {
            self.paste_calls.lock().await.clone()
        }

        async fn keystroke_calls(&self) -> Vec<String> {
            self.keystroke_calls.lock().await.clone()
        }
    }

    #[async_trait]
    impl FocusAutomation for MockAutomation {
        async fn inspect_focus(
            &self,
            _context: &FocusWindowContext,
            _timeout: Duration,
        ) -> Result<FocusCapabilities, AutomationError> {
            self.inspect_result.lock().await.clone()
        }

        async fn paste_via_clipboard(
            &self,
            contents: &str,
            _timeout: Duration,
        ) -> Result<(), AutomationError> {
            self.paste_calls.lock().await.push(contents.to_string());
            self.paste_result.lock().await.clone()
        }

        async fn simulate_keystrokes(
            &self,
            contents: &str,
            _timeout: Duration,
        ) -> Result<(), AutomationError> {
            self.keystroke_calls.lock().await.push(contents.to_string());
            self.keystroke_result.lock().await.clone()
        }
    }

    #[derive(Clone)]
    struct FlakyAutomation {
        attempts: Arc<Mutex<u8>>,
        succeed_on: u8,
    }

    impl FlakyAutomation {
        fn new(succeed_on: u8) -> Self {
            Self {
                attempts: Arc::new(Mutex::new(0)),
                succeed_on,
            }
        }

        async fn attempts(&self) -> u8 {
            *self.attempts.lock().await
        }
    }

    #[async_trait]
    impl FocusAutomation for FlakyAutomation {
        async fn inspect_focus(
            &self,
            _context: &FocusWindowContext,
            _timeout: Duration,
        ) -> Result<FocusCapabilities, AutomationError> {
            Ok(FocusCapabilities::writable_with_clipboard())
        }

        async fn paste_via_clipboard(
            &self,
            _contents: &str,
            _timeout: Duration,
        ) -> Result<(), AutomationError> {
            let mut guard = self.attempts.lock().await;
            *guard = guard.saturating_add(1);
            if *guard >= self.succeed_on {
                Ok(())
            } else {
                Err(AutomationError::Timeout)
            }
        }

        async fn simulate_keystrokes(
            &self,
            _contents: &str,
            _timeout: Duration,
        ) -> Result<(), AutomationError> {
            Err(AutomationError::channel_unavailable(
                "keystroke path unused in flaky test",
            ))
        }
    }

    #[tokio::test]
    async fn rejects_empty_transcript() {
        let automation =
            MockAutomation::with_capabilities(FocusCapabilities::writable_with_clipboard());
        let publisher = Publisher::with_automation(Arc::new(automation));
        let request = PublishRequest {
            transcript: "   ".to_string(),
            focus: FocusWindowContext::default(),
            fallback: FallbackStrategy::default(),
        };

        let result = publisher.publish(request).await;

        assert_eq!(result, Err(PublisherError::EmptyTranscript));
    }

    #[tokio::test]
    async fn uses_clipboard_channel_when_available() {
        let automation =
            MockAutomation::with_capabilities(FocusCapabilities::writable_with_clipboard());
        let publisher = Publisher::with_automation(Arc::new(automation.clone()));
        let context = FocusWindowContext::from_app_identifier("com.example.editor");
        let fallback = FallbackStrategy::ClipboardCopy;
        let mut request = PublishRequest {
            transcript: "润色稿内容".to_string(),
            focus: context.clone(),
            fallback: fallback.clone(),
        };

        request.focus.window_title = Some("Editor".into());
        request.focus.metadata = Some("rich-text".into());

        assert_eq!(request.focus.app_identifier, context.app_identifier);
        assert_eq!(request.focus.window_title.as_deref(), Some("Editor"));
        assert_eq!(request.focus.metadata.as_deref(), Some("rich-text"));
        assert!(matches!(request.fallback, FallbackStrategy::ClipboardCopy));

        let outcome = publisher.publish(request.clone()).await.unwrap();

        assert_eq!(outcome.status, PublisherStatus::Completed);
        assert_eq!(outcome.strategy, PublishStrategy::DirectInsert);
        assert_eq!(outcome.attempts, 1);
        assert!(outcome.fallback.is_none());
        assert!(outcome.failure.is_none());

        assert_eq!(automation.paste_calls().await, vec![request.transcript]);
        assert!(automation.keystroke_calls().await.is_empty());
    }

    #[tokio::test]
    async fn uses_keystroke_channel_when_clipboard_unavailable() {
        let automation =
            MockAutomation::with_capabilities(FocusCapabilities::writable_with_keystroke());
        let publisher = Publisher::with_automation(Arc::new(automation.clone()));
        let request = PublishRequest {
            transcript: "Hello".to_string(),
            focus: FocusWindowContext::default(),
            fallback: FallbackStrategy::default(),
        };

        let outcome = publisher.publish(request.clone()).await.unwrap();

        assert_eq!(outcome.status, PublisherStatus::Completed);
        assert_eq!(outcome.strategy, PublishStrategy::DirectInsert);
        assert_eq!(outcome.attempts, 1);
        assert!(automation.paste_calls().await.is_empty());
        assert_eq!(automation.keystroke_calls().await, vec![request.transcript]);
        assert!(outcome.failure.is_none());
    }

    #[tokio::test]
    async fn errors_when_focus_is_read_only() {
        let automation =
            MockAutomation::with_capabilities(FocusCapabilities::read_only("readonly"));
        let publisher = Publisher::with_automation(Arc::new(automation));
        let request = PublishRequest {
            transcript: "Hello".to_string(),
            focus: FocusWindowContext::default(),
            fallback: FallbackStrategy::default(),
        };

        let outcome = publisher.publish(request).await.unwrap();

        assert_eq!(outcome.status, PublisherStatus::Failed);
        assert_eq!(outcome.attempts, 1);
        let failure = outcome.failure.expect("failure details should be present");
        assert_eq!(failure.code, PublisherFailureCode::AutomationRejected);
        assert_eq!(failure.message, "readonly");
    }

    #[tokio::test]
    async fn propagates_inspection_error() {
        let automation = MockAutomation::with_inspect_error(AutomationError::PermissionDenied);
        let publisher = Publisher::with_automation(Arc::new(automation));
        let request = PublishRequest {
            transcript: "Hello".to_string(),
            focus: FocusWindowContext::default(),
            fallback: FallbackStrategy::default(),
        };

        let outcome = publisher.publish(request).await.unwrap();

        assert_eq!(outcome.status, PublisherStatus::Failed);
        assert_eq!(outcome.attempts, 2);
        let failure = outcome.failure.expect("failure details");
        assert_eq!(failure.code, PublisherFailureCode::PermissionDenied);
        assert_eq!(failure.message, "accessibility permission denied");
    }

    #[tokio::test]
    async fn reports_focus_lost_failure_when_inspection_cannot_find_target() {
        let automation = MockAutomation::with_inspect_error(AutomationError::focus_not_found());
        let publisher = Publisher::with_automation(Arc::new(automation));
        let request = PublishRequest {
            transcript: "Hello".to_string(),
            focus: FocusWindowContext::default(),
            fallback: FallbackStrategy::default(),
        };

        let outcome = publisher.publish(request).await.unwrap();

        assert_eq!(outcome.status, PublisherStatus::Failed);
        assert_eq!(outcome.attempts, 2);
        let failure = outcome.failure.expect("failure details");
        assert_eq!(failure.code, PublisherFailureCode::FocusLost);
        assert_eq!(failure.message, "focus window not found");
    }

    #[tokio::test]
    async fn propagates_insertion_error() {
        let automation =
            MockAutomation::with_capabilities(FocusCapabilities::writable_with_clipboard());
        automation.set_paste_error(AutomationError::Timeout).await;
        let publisher = Publisher::with_automation(Arc::new(automation.clone()));
        let request = PublishRequest {
            transcript: "Hello".to_string(),
            focus: FocusWindowContext::default(),
            fallback: FallbackStrategy::default(),
        };

        let outcome = publisher.publish(request).await.unwrap();

        assert_eq!(outcome.status, PublisherStatus::Failed);
        assert_eq!(outcome.attempts, 2);
        let failure = outcome.failure.expect("failure details");
        assert_eq!(failure.code, PublisherFailureCode::Timeout);
        assert_eq!(failure.message, "operation timed out");
        assert_eq!(automation.paste_calls().await.len(), 2);
    }

    #[tokio::test]
    async fn retries_until_success_within_limit() {
        let automation = FlakyAutomation::new(2);
        let publisher = Publisher::new(PublisherConfig::default(), Arc::new(automation.clone()));
        let request = PublishRequest {
            transcript: "Hello".to_string(),
            focus: FocusWindowContext::default(),
            fallback: FallbackStrategy::default(),
        };

        let outcome = publisher.publish(request).await.unwrap();

        assert_eq!(outcome.status, PublisherStatus::Completed);
        assert_eq!(outcome.attempts, 2);
        assert!(outcome.failure.is_none());
        assert_eq!(automation.attempts().await, 2);
    }

    #[tokio::test]
    async fn fails_after_exhausting_retries() {
        let automation = FlakyAutomation::new(5);
        let mut config = PublisherConfig::default();
        config.max_retry = 1;
        let publisher = Publisher::new(config, Arc::new(automation.clone()));
        let request = PublishRequest {
            transcript: "Hello".to_string(),
            focus: FocusWindowContext::default(),
            fallback: FallbackStrategy::default(),
        };

        let outcome = publisher.publish(request).await.unwrap();

        assert_eq!(outcome.status, PublisherStatus::Failed);
        assert_eq!(outcome.attempts, 2);
        let failure = outcome.failure.expect("failure details");
        assert_eq!(failure.code, PublisherFailureCode::Timeout);
        assert_eq!(failure.message, "operation timed out");
        assert_eq!(automation.attempts().await, 2);
    }

    #[tokio::test]
    async fn errors_when_no_automation_channel_available() {
        let automation = MockAutomation::with_capabilities(FocusCapabilities {
            is_writable: true,
            supports_clipboard_paste: false,
            supports_keystroke_injection: false,
            reason: Some("no channel".into()),
        });
        let publisher = Publisher::with_automation(Arc::new(automation));
        let request = PublishRequest {
            transcript: "Hello".to_string(),
            focus: FocusWindowContext::default(),
            fallback: FallbackStrategy::default(),
        };

        let outcome = publisher.publish(request).await.unwrap();

        assert_eq!(outcome.status, PublisherStatus::Failed);
        assert_eq!(outcome.attempts, 1);
        let failure = outcome.failure.expect("failure details");
        assert_eq!(failure.code, PublisherFailureCode::ChannelUnavailable);
        assert_eq!(failure.message, "no channel");
    }

    #[tokio::test]
    async fn exposes_config_defaults() {
        let config = PublisherConfig::default();
        assert_eq!(config.direct_insert_timeout, Duration::from_millis(400));
        assert_eq!(config.fallback_timeout, Duration::from_millis(200));
        assert_eq!(config.max_retry, 1);

        let automation =
            MockAutomation::with_capabilities(FocusCapabilities::writable_with_clipboard());
        let publisher = Publisher::new(config.clone(), Arc::new(automation));
        assert_eq!(publisher.config().max_retry, config.max_retry);
    }

    #[test]
    fn fallback_variants_are_constructible() {
        let none = FallbackStrategy::None;
        let clipboard = FallbackStrategy::ClipboardCopy;
        let notify = FallbackStrategy::NotifyOnly;

        assert!(matches!(none, FallbackStrategy::None));
        assert!(matches!(clipboard, FallbackStrategy::ClipboardCopy));
        assert!(matches!(notify, FallbackStrategy::NotifyOnly));
    }

    #[test]
    fn can_build_focus_context_with_metadata() {
        let mut context = FocusWindowContext::from_app_identifier("app");
        context.window_title = Some("Title".into());
        context.metadata = Some("insert".into());

        assert_eq!(context.app_identifier.as_deref(), Some("app"));
        assert_eq!(context.window_title.as_deref(), Some("Title"));
        assert_eq!(context.metadata.as_deref(), Some("insert"));
    }

    #[test]
    fn publish_outcome_deferred_helper() {
        let outcome = PublishOutcome::deferred(
            PublishStrategy::ClipboardFallback,
            Some(FallbackStrategy::ClipboardCopy),
        );

        assert_eq!(outcome.status, PublisherStatus::Deferred);
        assert_eq!(outcome.strategy, PublishStrategy::ClipboardFallback);
        assert_eq!(outcome.attempts, 0);
        assert!(matches!(
            outcome.fallback,
            Some(FallbackStrategy::ClipboardCopy)
        ));
        assert!(outcome.failure.is_none());
    }

    #[test]
    fn publisher_status_variants_constructible() {
        assert!(matches!(
            PublisherStatus::Completed,
            PublisherStatus::Completed
        ));
        assert!(matches!(
            PublisherStatus::Deferred,
            PublisherStatus::Deferred
        ));
        assert!(matches!(PublisherStatus::Failed, PublisherStatus::Failed));

        assert!(matches!(
            PublishStrategy::DirectInsert,
            PublishStrategy::DirectInsert
        ));
        assert!(matches!(
            PublishStrategy::ClipboardFallback,
            PublishStrategy::ClipboardFallback
        ));
        assert!(matches!(
            PublishStrategy::NotifyOnly,
            PublishStrategy::NotifyOnly
        ));
    }
}
