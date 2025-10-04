#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::sync::{Arc, Mutex};

use serde::Serialize;
use tauri::{Manager, State};

#[derive(Debug, Default, Serialize, Clone)]
struct SessionStatus {
    phase: String,
    detail: String,
}

type SharedStatus = Arc<Mutex<SessionStatus>>;

#[tauri::command]
fn session_status(state: State<SharedStatus>) -> Result<SessionStatus, String> {
    state
        .lock()
        .map(|status| (*status).clone())
        .map_err(|err| format!("failed to read session status: {err}"))
}

#[tauri::command]
fn update_session_status(phase: String, detail: String, state: State<SharedStatus>) -> Result<(), String> {
    let mut guard = state
        .lock()
        .map_err(|err| format!("failed to lock session status: {err}"))?;
    guard.phase = phase;
    guard.detail = detail;
    Ok(())
}

fn main() {
    tauri::Builder::default()
        .manage(Arc::new(Mutex::new(SessionStatus {
            phase: "Idle".into(),
            detail: "Core service bridge not connected".into(),
        })))
        .invoke_handler(tauri::generate_handler![session_status, update_session_status])
        .setup(|app| {
            let window = app.get_window("main").expect("main window should exist");
            window.set_title("Flowwisper Fn").ok();
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running Flowwisper desktop shell");
}
