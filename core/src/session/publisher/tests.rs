use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::Mutex;

use super::{
    AutomationError, FallbackStrategy, FocusAutomation, FocusCapabilities, FocusWindowContext,
    PublishOutcome, PublishRequest, PublishStrategy, Publisher, PublisherConfig, PublisherError,
    PublisherFailureCode, PublisherStatus,
};

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
    let automation = MockAutomation::with_capabilities(FocusCapabilities::read_only("readonly"));
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
