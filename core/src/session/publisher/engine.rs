use std::sync::Arc;

use async_trait::async_trait;

use crate::session::publisher::automation::{FocusAutomation, SystemFocusAutomation};
use crate::session::publisher::error::PublisherError;
use crate::session::publisher::types::{
    PublishOutcome, PublishRequest, PublishStrategy, PublisherConfig, PublisherFailure,
    PublisherFailureCode,
};

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
