use std::time::Duration;

use crate::session::publisher::automation::AutomationError;
use crate::session::publisher::error::PublisherError;

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

impl PublisherStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            PublisherStatus::Completed => "completed",
            PublisherStatus::Deferred => "deferred",
            PublisherStatus::Failed => "failed",
        }
    }
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

impl PublishStrategy {
    pub fn as_str(&self) -> &'static str {
        match self {
            PublishStrategy::DirectInsert => "direct_insert",
            PublishStrategy::ClipboardFallback => "clipboard_fallback",
            PublishStrategy::NotifyOnly => "notify_only",
        }
    }
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
