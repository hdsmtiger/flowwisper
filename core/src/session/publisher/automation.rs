use std::time::Duration;

use async_trait::async_trait;
use thiserror::Error;

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
        context: &crate::session::publisher::types::FocusWindowContext,
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

#[derive(Default)]
pub struct SystemFocusAutomation;

#[async_trait]
impl FocusAutomation for SystemFocusAutomation {
    async fn inspect_focus(
        &self,
        _context: &crate::session::publisher::types::FocusWindowContext,
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
