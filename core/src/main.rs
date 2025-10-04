mod audio;
mod orchestrator;
mod persistence;
mod session;
mod telemetry;

use anyhow::Result;
use session::SessionManager;
use telemetry::init_tracing;

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();

    let manager = SessionManager::new();
    manager.run().await
}
