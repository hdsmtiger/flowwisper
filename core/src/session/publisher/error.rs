use thiserror::Error;

use crate::session::publisher::automation::AutomationError;

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
