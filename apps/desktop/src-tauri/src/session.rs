use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};
use tauri::{AppHandle, Emitter};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionStatus {
    pub phase: String,
    pub detail: String,
    pub timestamp_ms: u128,
}

impl SessionStatus {
    fn new(phase: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            phase: phase.into(),
            detail: detail.into(),
            timestamp_ms: Self::now_ms(),
        }
    }

    fn now_ms() -> u128 {
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0)
    }
}

impl Default for SessionStatus {
    fn default() -> Self {
        Self::new(
            "Idle",
            "Core service bridge not connected — awaiting initialization",
        )
    }
}

#[derive(Debug, Clone)]
pub struct SessionStateManager {
    current: Arc<Mutex<SessionStatus>>,
    history: Arc<Mutex<Vec<SessionStatus>>>,
}

impl SessionStateManager {
    pub fn new() -> Self {
        Self {
            current: Arc::new(Mutex::new(SessionStatus::default())),
            history: Arc::new(Mutex::new(vec![SessionStatus::default()])),
        }
    }

    pub fn snapshot(&self) -> Result<SessionStatus, String> {
        self.current
            .lock()
            .map(|status| status.clone())
            .map_err(|err| format!("failed to read session status: {err}"))
    }

    pub fn timeline(&self) -> Result<Vec<SessionStatus>, String> {
        self.history
            .lock()
            .map(|history| history.clone())
            .map_err(|err| format!("failed to read session timeline: {err}"))
    }

    pub fn transition(
        &self,
        phase: impl Into<String>,
        detail: impl Into<String>,
    ) -> Result<SessionStatus, String> {
        let updated = SessionStatus::new(phase, detail);
        {
            let mut guard = self
                .current
                .lock()
                .map_err(|err| format!("failed to update session status: {err}"))?;
            *guard = updated.clone();
        }

        let mut history = self
            .history
            .lock()
            .map_err(|err| format!("failed to update session timeline: {err}"))?;
        history.push(updated.clone());
        if history.len() > 50 {
            let drain = history.len() - 50;
            history.drain(0..drain);
        }
        Ok(updated)
    }

    pub fn transition_and_emit(
        &self,
        app: &AppHandle,
        phase: impl Into<String>,
        detail: impl Into<String>,
    ) -> Result<SessionStatus, String> {
        let status = self.transition(phase, detail)?;
        app.emit("session://state", &status)
            .map_err(|err| format!("failed to emit session state: {err}"))?;
        Ok(status)
    }

    pub fn drive_preroll(
        &self,
        app: &AppHandle,
        priming_detail: impl Into<String>,
        preroll_detail: impl Into<String>,
    ) {
        let manager = self.clone();
        let handle = app.clone();
        let priming = priming_detail.into();
        let preroll = preroll_detail.into();
        let send_preroll = !preroll.trim().is_empty();
        std::thread::spawn(move || {
            let _ = manager.transition_and_emit(&handle, "Priming", priming);
            if send_preroll {
                std::thread::sleep(Duration::from_millis(120));
                let _ = manager.transition_and_emit(&handle, "PreRoll", preroll);
            }
        });
    }

    pub fn mark_processing(&self, app: AppHandle) {
        let manager = self.clone();
        std::thread::spawn(move || {
            let _ =
                manager.transition_and_emit(&app, "Processing", "Awaiting core-service handshake");
        });
    }

    pub fn complete_ready(&self, app: AppHandle) {
        let _ = self.transition_and_emit(&app, "Ready", "Session ready for hands-free capture");
    }
}
