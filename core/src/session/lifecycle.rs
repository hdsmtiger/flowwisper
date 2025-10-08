//! 会话生命周期广播负载定义。

use std::time::SystemTime;

use super::publisher::{FallbackStrategy, PublishOutcome, PublishStrategy, PublisherStatus};

/// 会话状态机的阶段划分。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionLifecyclePhase {
    Idle,
    PreRoll,
    Recording,
    Processing,
    Publishing,
    Completed,
    Failed,
}

/// 生命周期事件的附加信息。
#[derive(Debug, Clone)]
pub enum SessionLifecyclePayload {
    None,
    Publishing(PublishingPayload),
    Completed(CompletionPayload),
    Failed(FailurePayload),
}

impl Default for SessionLifecyclePayload {
    fn default() -> Self {
        SessionLifecyclePayload::None
    }
}

/// 发布阶段的状态快照。
#[derive(Debug, Clone)]
pub struct PublishingPayload {
    pub attempt: u8,
    pub strategy: PublishStrategy,
    pub fallback: Option<FallbackStrategy>,
}

/// 完成阶段的结果摘要。
#[derive(Debug, Clone)]
pub struct CompletionPayload {
    pub outcome: PublishOutcome,
}

/// 失败阶段的上下文信息。
#[derive(Debug, Clone)]
pub struct FailurePayload {
    pub attempts: u8,
    pub error: String,
    pub code: Option<String>,
    pub fallback: Option<FallbackStrategy>,
}

/// 生命周期事件。
#[derive(Debug, Clone)]
pub struct SessionLifecycleUpdate {
    pub session_id: String,
    pub phase: SessionLifecyclePhase,
    pub issued_at: SystemTime,
    pub payload: SessionLifecyclePayload,
}

impl SessionLifecycleUpdate {
    /// 构造一个空载荷的事件。
    pub fn new<S: Into<String>>(session_id: S, phase: SessionLifecyclePhase) -> Self {
        Self {
            session_id: session_id.into(),
            phase,
            issued_at: SystemTime::now(),
            payload: SessionLifecyclePayload::None,
        }
    }

    /// 声明当前处于 Publishing 阶段。
    pub fn publishing<S: Into<String>>(
        session_id: S,
        attempt: u8,
        strategy: PublishStrategy,
        fallback: Option<FallbackStrategy>,
    ) -> Self {
        Self {
            session_id: session_id.into(),
            phase: SessionLifecyclePhase::Publishing,
            issued_at: SystemTime::now(),
            payload: SessionLifecyclePayload::Publishing(PublishingPayload {
                attempt,
                strategy,
                fallback,
            }),
        }
    }

    /// 声明会话完成并携带发布结果。
    pub fn completed<S: Into<String>>(session_id: S, outcome: PublishOutcome) -> Self {
        Self {
            session_id: session_id.into(),
            phase: SessionLifecyclePhase::Completed,
            issued_at: SystemTime::now(),
            payload: SessionLifecyclePayload::Completed(CompletionPayload { outcome }),
        }
    }

    /// 声明发布失败。
    pub fn failed<S: Into<String>>(
        session_id: S,
        attempts: u8,
        error: impl Into<String>,
        code: Option<String>,
        fallback: Option<FallbackStrategy>,
    ) -> Self {
        Self {
            session_id: session_id.into(),
            phase: SessionLifecyclePhase::Failed,
            issued_at: SystemTime::now(),
            payload: SessionLifecyclePayload::Failed(FailurePayload {
                attempts,
                error: error.into(),
                code,
                fallback,
            }),
        }
    }
}

impl PublisherStatus {
    /// 将 PublisherStatus 映射到生命周期阶段。
    pub fn as_phase(&self) -> SessionLifecyclePhase {
        match self {
            PublisherStatus::Completed | PublisherStatus::Deferred => {
                SessionLifecyclePhase::Completed
            }
            PublisherStatus::Failed => SessionLifecyclePhase::Failed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn publishing_helper_sets_payload() {
        let update = SessionLifecycleUpdate::publishing(
            "session",
            1,
            PublishStrategy::DirectInsert,
            Some(FallbackStrategy::ClipboardCopy),
        );

        match update.payload {
            SessionLifecyclePayload::Publishing(payload) => {
                assert_eq!(payload.attempt, 1);
                assert_eq!(payload.strategy, PublishStrategy::DirectInsert);
                assert!(matches!(
                    payload.fallback,
                    Some(FallbackStrategy::ClipboardCopy)
                ));
            }
            other => panic!("unexpected payload: {other:?}"),
        }
    }

    #[test]
    fn completion_helper_wraps_outcome() {
        let outcome = PublishOutcome::completed();
        let update = SessionLifecycleUpdate::completed("session", outcome.clone());

        match update.payload {
            SessionLifecyclePayload::Completed(payload) => {
                assert_eq!(payload.outcome, outcome);
            }
            other => panic!("unexpected payload: {other:?}"),
        }
    }

    #[test]
    fn failed_helper_sets_error() {
        let update = SessionLifecycleUpdate::failed(
            "session",
            2,
            "permission denied",
            Some("permission_denied".into()),
            Some(FallbackStrategy::NotifyOnly),
        );

        match update.payload {
            SessionLifecyclePayload::Failed(payload) => {
                assert_eq!(payload.attempts, 2);
                assert_eq!(payload.error, "permission denied");
                assert_eq!(payload.code.as_deref(), Some("permission_denied"));
                assert!(matches!(
                    payload.fallback,
                    Some(FallbackStrategy::NotifyOnly)
                ));
            }
            other => panic!("unexpected payload: {other:?}"),
        }
    }

    #[test]
    fn publisher_status_to_phase_mapping() {
        assert_eq!(
            PublisherStatus::Completed.as_phase(),
            SessionLifecyclePhase::Completed
        );
        assert_eq!(
            PublisherStatus::Deferred.as_phase(),
            SessionLifecyclePhase::Completed
        );
        assert_eq!(
            PublisherStatus::Failed.as_phase(),
            SessionLifecyclePhase::Failed
        );
    }
}
