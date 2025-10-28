pub mod automation;
pub mod engine;
pub mod error;
pub mod types;

pub use automation::{AutomationError, FocusAutomation, FocusCapabilities};
pub use engine::{Publisher, SessionPublisher};
pub use error::PublisherError;
pub use types::{
    FallbackStrategy, FocusWindowContext, PublishOutcome, PublishRequest, PublishStrategy,
    PublisherConfig, PublisherFailure, PublisherFailureCode, PublisherStatus,
};

#[cfg(test)]
mod tests;
