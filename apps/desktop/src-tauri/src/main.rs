#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use serde::{Deserialize, Serialize};
use std::{path::PathBuf, time::Duration};
use tauri::{
    AppHandle, CustomMenuItem, Manager, State, SystemTray, SystemTrayEvent, SystemTrayMenu,
    SystemTrayMenuItem,
};

mod audio;
mod history;
mod hotkey;
mod native_probe;
mod session;

use audio::{
    calibrate_device, check_accessibility_permission as check_system_accessibility_permission,
    existing_calibration, list_devices, open_accessibility_settings, open_microphone_settings,
    prime_waveform_bridge,
    request_accessibility_permission as request_system_accessibility_permission,
    request_microphone_permission as request_system_microphone_permission, run_device_check,
    CalibrationComputation, DeviceTestReport, FrameWindowSetting,
};
use flowwisper_core::session::history::{
    AccuracyUpdate, HistoryEntry, HistoryPage, HistoryPostAction, HistoryQuery,
};
use hotkey::{
    load_hotkey_config, load_or_create_hmac_key, AppState, FnProbeResult, HotkeyBinding,
    HotkeyCompatibilityLayer, HotkeySource, TutorialStatus,
};
use session::{
    InsertionResult, PublishNotice, PublishingUpdate, SessionStatus, TranscriptSentenceSelection,
    TranscriptStreamEvent,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct HotkeyValidationResult {
    combination: String,
    conflict_with: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PersistHotkeyRequest {
    combination: String,
    source: HotkeySource,
    reason: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct PermissionResponse {
    granted: bool,
    manual_hint: Option<String>,
    platform: String,
    detail: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct PermissionStatusSummary {
    microphone: bool,
    accessibility: bool,
}

#[derive(Debug, Clone, Serialize)]
struct TutorialCompletionSummary {
    finished: bool,
    status: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct AudioInputDevice {
    id: String,
    label: String,
    kind: String,
    preferred: bool,
}

#[derive(Debug, Clone, Serialize)]
struct CalibrationResult {
    device_id: String,
    device_label: String,
    recommended_threshold: f32,
    applied_threshold: f32,
    noise_floor_db: f32,
    sample_window_ms: u32,
    frame_window_ms: u32,
    mode: String,
    updated_at_ms: Option<u128>,
    noise_alert: bool,
    noise_hint: Option<String>,
    strong_noise_mode: bool,
}

#[derive(Debug, Clone, Serialize)]
struct AudioDiagnostics {
    device_id: String,
    device_label: String,
    duration_ms: u32,
    sample_rate: u32,
    snr_db: f32,
    peak_dbfs: f32,
    rms_dbfs: f32,
    noise_floor_db: f32,
    noise_alert: bool,
    noise_hint: Option<String>,
    waveform: Vec<f32>,
    sample_token: String,
    frame_window_ms: u32,
}

#[derive(Debug, Deserialize)]
struct TranscriptSelectionRequest {
    selections: Vec<TranscriptSentenceSelection>,
}

#[derive(Debug, Clone, Serialize)]
struct EnginePreference {
    choice: Option<String>,
    recommended: String,
    privacy_notice: String,
}

const HOTKEY_TRAY_ID: &str = "hotkey_display";

fn build_tray() -> SystemTray {
    let tray_menu = SystemTrayMenu::new()
        .add_item(CustomMenuItem::new(HOTKEY_TRAY_ID, "当前热键: Fn").disabled())
        .add_native_item(SystemTrayMenuItem::Separator)
        .add_item(CustomMenuItem::new("quit", "退出").accelerator("CmdOrCtrl+Q"));

    SystemTray::new().with_menu(tray_menu)
}

fn resolve_config_path(app: &AppHandle) -> Result<PathBuf, String> {
    let mut path = app
        .path_resolver()
        .app_config_dir()
        .ok_or_else(|| "missing config directory".to_string())?;
    path.push("hotkey.json");
    Ok(path)
}

fn update_tray_hotkey(app: &AppHandle, binding: &HotkeyBinding) {
    if let Some(tray) = app.tray_handle().get_item(HOTKEY_TRAY_ID).ok() {
        let _ = tray.set_title(format!("当前热键: {}", binding.combination));
    }
}

impl HotkeyCompatibilityLayer {
    fn detect_conflict(app: &AppHandle, combination: &str) -> Result<Option<String>, String> {
        if let Some(value) = Self::RESERVED
            .iter()
            .copied()
            .find(|reserved| reserved.eq_ignore_ascii_case(combination))
        {
            return Ok(Some(value.to_string()));
        }

        #[cfg(any(target_os = "macos", target_os = "windows"))]
        {
            let mut manager = app.global_shortcut();
            if manager
                .is_registered(combination)
                .map_err(|err| format!("failed to query shortcut: {err}"))?
            {
                return Ok(Some(format!("{combination}")));
            }

            match manager.register(combination, || {}) {
                Ok(()) => {
                    let _ = manager.unregister(combination);
                    Ok(None)
                }
                Err(err) => {
                    let message = err.to_string();
                    if message.to_lowercase().contains("already") {
                        Ok(Some(combination.to_string()))
                    } else {
                        Err(format!("无法在系统中注册组合 {combination}: {message}"))
                    }
                }
            }
        }

        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            let _ = app;
            let _ = combination;
            Ok(None)
        }
    }
}

#[tauri::command]
fn session_status(state: State<AppState>) -> Result<SessionStatus, String> {
    state.session.snapshot()
}

#[tauri::command]
fn session_timeline(state: State<AppState>) -> Result<Vec<SessionStatus>, String> {
    state.session.timeline()
}

#[tauri::command]
fn session_transcript_log(state: State<AppState>) -> Result<Vec<TranscriptStreamEvent>, String> {
    state.session.transcript_log()
}

#[tauri::command]
fn session_publish_update(
    app: AppHandle,
    state: State<AppState>,
    update: PublishingUpdate,
) -> Result<(), String> {
    state.session.emit_publishing_update(&app, update)
}

#[tauri::command]
fn session_publish_result(
    app: AppHandle,
    state: State<AppState>,
    result: InsertionResult,
) -> Result<(), String> {
    state.session.emit_insertion_result(&app, result)
}

#[tauri::command]
fn session_publish_notice(
    app: AppHandle,
    state: State<AppState>,
    notice: PublishNotice,
) -> Result<(), String> {
    state.session.emit_publish_notice(&app, notice)
}

#[tauri::command]
fn session_publish_history(state: State<AppState>) -> Result<Vec<PublishingUpdate>, String> {
    state.session.publishing_history()
}

#[tauri::command]
fn session_publish_results(state: State<AppState>) -> Result<Vec<InsertionResult>, String> {
    state.session.insertion_history()
}

#[tauri::command]
fn session_publish_notices(state: State<AppState>) -> Result<Vec<PublishNotice>, String> {
    state.session.publish_notice_history()
}

#[tauri::command]
fn session_notice_center_history(state: State<AppState>) -> Result<Vec<PublishNotice>, String> {
    state.session.publish_notice_history()
}

#[tauri::command]
async fn session_history_search(query: HistoryQuery) -> Result<HistoryPage, String> {
    history::search_history(query).await
}

#[tauri::command]
async fn session_history_entry(session_id: String) -> Result<Option<HistoryEntry>, String> {
    history::load_history(session_id).await
}

#[tauri::command]
async fn session_history_mark_accuracy(update: AccuracyUpdate) -> Result<(), String> {
    history::mark_accuracy(update).await
}

#[tauri::command]
async fn session_history_append_action(
    request: history::HistoryActionRequest,
) -> Result<Vec<HistoryPostAction>, String> {
    history::append_action(request.session_id, request.action, request.detail).await
}

#[tauri::command]
fn session_transcript_apply_selection(
    app: AppHandle,
    state: State<AppState>,
    request: TranscriptSelectionRequest,
) -> Result<(), String> {
    state
        .session
        .apply_transcript_selection(&app, request.selections)
}

#[tauri::command]
fn prime_session_preroll(app: AppHandle, state: State<AppState>) -> Result<SessionStatus, String> {
    state
        .session
        .transition_and_emit(&app, "Idle", "Initializing onboarding sequence")?;
    state.session.snapshot()
}

#[tauri::command]
fn mark_session_processing(
    app: AppHandle,
    state: State<AppState>,
) -> Result<SessionStatus, String> {
    state.session.mark_processing(app);
    state.session.snapshot()
}

#[tauri::command]
fn complete_session_bootstrap(
    app: AppHandle,
    state: State<AppState>,
) -> Result<SessionStatus, String> {
    state
        .mark_tutorial_complete()
        .map_err(|err| format!("failed to persist tutorial completion: {err}"))?;
    state.session.transition_and_emit(
        &app,
        "Completed",
        "Onboarding tutorial finished (completed)",
    )?;
    state.session.snapshot()
}

#[tauri::command]
fn start_fn_probe(app: AppHandle, state: State<AppState>) -> Result<FnProbeResult, String> {
    let meter_device = state.selected_microphone();
    let meter_app = app.clone();
    let frame_window = state.frame_window_mode();
    std::thread::spawn(move || {
        let _ = prime_waveform_bridge(
            meter_app,
            meter_device,
            Duration::from_millis(1200),
            frame_window,
        );
    });
    state
        .session
        .drive_preroll(&app, "Fn 预热中，等待驱动回调", "");
    let result = HotkeyCompatibilityLayer::probe_fn();
    if let Ok(mut guard) = state.hotkey.lock() {
        guard.last_probe = Some(result.clone());
    }
    state
        .append_probe_log(&result)
        .map_err(|err| format!("failed to persist probe telemetry: {err}"))?;
    let frame_mode = if !result.supported {
        state.set_frame_window(
            FrameWindowSetting::Fallback,
            Some("Fn 捕获失败，保持 100ms 帧窗口".into()),
        )
    } else if matches!(result.within_sla, Some(false)) {
        let reason = result
            .latency_ms
            .map(|latency| format!("Fn 延迟 {latency}ms 超出 400ms SLA"))
            .unwrap_or_else(|| "Fn 延迟超出 400ms SLA".into());
        state.set_frame_window(FrameWindowSetting::Fallback, Some(reason))
    } else {
        state.set_frame_window(FrameWindowSetting::Default, None)
    };
    let frame_detail = state
        .frame_window_reason()
        .map(|reason| format!("帧窗口 {}ms（{}）", frame_mode.duration_ms(), reason))
        .unwrap_or_else(|| format!("帧窗口 {}ms", frame_mode.duration_ms()));
    if result.supported {
        let priming_detail = result
            .interface
            .as_ref()
            .map(|iface| format!("Fn 捕获接口: {iface}"))
            .unwrap_or_else(|| "Fn 捕获回调已触发".into());
        let preroll_detail = if let Some(latency) = result.latency_ms {
            let within = match result.within_sla {
                Some(true) => "满足 400ms SLA",
                Some(false) => "超出 400ms SLA",
                None => "SLA 未知",
            };
            format!("Fn 驱动延迟 {latency}ms（{within}） | {frame_detail}",)
        } else {
            format!("Fn 驱动延迟未知，已记录回调 | {frame_detail}")
        };
        state.session.transition_and_emit(
            &app,
            "Priming",
            format!("{priming_detail} | {frame_detail}"),
        )?;
        state
            .session
            .transition_and_emit(&app, "PreRoll", preroll_detail.clone())?;
        state
            .session
            .transition_and_emit(&app, "Ready", "Fn key captured; session ready")?;
    } else {
        let preroll_detail = result
            .reason
            .clone()
            .unwrap_or_else(|| "Fn 捕获失败，已建议录制备用组合".into());
        state.session.transition_and_emit(
            &app,
            "PreRoll",
            format!("{preroll_detail} | {frame_detail}"),
        )?;
        state.session.transition_and_emit(
            &app,
            "Fallback",
            result
                .reason
                .clone()
                .map(|reason| {
                    if reason.contains("Fn") {
                        format!("{reason} | {frame_detail}")
                    } else {
                        format!("Fn 捕获失败：{reason} | {frame_detail}")
                    }
                })
                .unwrap_or_else(|| format!("Fn 捕获失败，已建议录制备用组合 | {frame_detail}")),
        )?;
    }
    Ok(result)
}

#[tauri::command]
fn validate_custom_hotkey(
    app: AppHandle,
    combination: String,
) -> Result<HotkeyValidationResult, String> {
    if combination.trim().is_empty() {
        return Err("组合不能为空".into());
    }

    let conflict = HotkeyCompatibilityLayer::detect_conflict(&app, &combination)?;

    Ok(HotkeyValidationResult {
        combination,
        conflict_with: conflict,
    })
}

#[tauri::command]
fn list_hotkey_conflicts() -> Vec<String> {
    HotkeyCompatibilityLayer::conflicts()
}

#[tauri::command]
fn request_microphone_permission(
    app: AppHandle,
    state: State<AppState>,
) -> Result<PermissionResponse, String> {
    state
        .session
        .transition_and_emit(&app, "PermissionPrompt", "Requesting microphone access")?;

    let platform = std::env::consts::OS.to_string();
    let permission = request_system_microphone_permission()?;

    if permission.granted {
        state.session.transition_and_emit(
            &app,
            "PermissionGranted",
            "Microphone permission granted",
        )?;
    } else {
        let detail = permission
            .manual_hint
            .clone()
            .unwrap_or_else(|| "需要用户手动授予麦克风权限".into());
        state
            .session
            .transition_and_emit(&app, "PermissionRequired", detail)?;
    }

    state
        .update_permission_status("microphone", permission.granted)
        .map_err(|err| format!("failed to persist microphone permission: {err}"))?;

    Ok(PermissionResponse {
        granted: permission.granted,
        manual_hint: permission.manual_hint,
        platform,
        detail: permission.detail,
    })
}

#[tauri::command]
fn request_accessibility_permission(
    app: AppHandle,
    state: State<AppState>,
) -> Result<PermissionResponse, String> {
    state.session.transition_and_emit(
        &app,
        "PermissionPrompt",
        "Requesting accessibility access",
    )?;

    let platform = std::env::consts::OS.to_string();
    let permission = request_system_accessibility_permission()?;

    if permission.granted {
        state.session.transition_and_emit(
            &app,
            "PermissionGranted",
            "Accessibility permission granted",
        )?;
    } else {
        let detail = permission
            .manual_hint
            .clone()
            .unwrap_or_else(|| "需要辅助功能权限以捕获 Fn 键".into());
        state
            .session
            .transition_and_emit(&app, "PermissionRequired", detail)?;
    }

    state
        .update_permission_status("accessibility", permission.granted)
        .map_err(|err| format!("failed to persist accessibility permission: {err}"))?;

    Ok(PermissionResponse {
        granted: permission.granted,
        manual_hint: permission.manual_hint,
        platform,
        detail: permission.detail,
    })
}

#[tauri::command]
fn check_accessibility_permission(state: State<AppState>) -> Result<PermissionResponse, String> {
    let platform = std::env::consts::OS.to_string();
    let permission = check_system_accessibility_permission()?;

    state
        .update_permission_status("accessibility", permission.granted)
        .map_err(|err| format!("failed to persist accessibility permission: {err}"))?;

    Ok(PermissionResponse {
        granted: permission.granted,
        manual_hint: permission.manual_hint,
        platform,
        detail: permission.detail,
    })
}

#[tauri::command]
fn permission_status(state: State<AppState>) -> Result<PermissionStatusSummary, String> {
    let status = state.permission_status();
    Ok(PermissionStatusSummary {
        microphone: status.microphone,
        accessibility: status.accessibility,
    })
}

#[tauri::command]
fn list_audio_inputs(
    app: AppHandle,
    state: State<AppState>,
) -> Result<Vec<AudioInputDevice>, String> {
    state.session.transition_and_emit(
        &app,
        "DeviceDiscovery",
        "Enumerating audio input devices",
    )?;

    let devices = list_devices()?;
    let devices: Vec<AudioInputDevice> = devices
        .into_iter()
        .map(|device| AudioInputDevice {
            id: device.id,
            label: device.label,
            kind: device.kind,
            preferred: device.preferred,
        })
        .collect();

    state.session.transition_and_emit(
        &app,
        "DeviceReady",
        format!("Discovered {} devices", devices.len()),
    )?;

    Ok(devices)
}

#[tauri::command]
fn calibrate_noise_floor(
    app: AppHandle,
    state: State<AppState>,
    device_id: Option<String>,
) -> Result<CalibrationResult, String> {
    state
        .session
        .transition_and_emit(&app, "Calibration", "Measuring ambient noise")?;

    let device_key = device_id.clone();
    let (computation, saved) = calibrate_device(device_key.as_deref(), &state)?;
    let persisted = existing_calibration(&state, &computation.device_id).unwrap_or(saved.clone());

    let result = CalibrationResult {
        device_id: computation.device_id.clone(),
        device_label: computation.device_label.clone(),
        recommended_threshold: persisted
            .recommended_threshold
            .unwrap_or(computation.recommended_threshold),
        applied_threshold: persisted.threshold,
        noise_floor_db: persisted.noise_floor_db,
        sample_window_ms: persisted.sample_window_ms,
        frame_window_ms: persisted
            .frame_window_ms
            .unwrap_or(state.frame_window_mode().duration_ms()),
        mode: persisted.mode.to_string(),
        updated_at_ms: persisted.updated_at_ms,
        noise_alert: persisted.noise_alert,
        noise_hint: persisted.noise_hint.clone(),
        strong_noise_mode: persisted.strong_noise_mode,
    };

    state.session.transition_and_emit(
        &app,
        "CalibrationComplete",
        format!(
            "Noise floor {:.1} dB, recommended threshold {:.2} | 帧窗口 {}ms",
            result.noise_floor_db, result.recommended_threshold, result.frame_window_ms
        ),
    )?;

    Ok(result)
}

#[tauri::command]
fn get_device_calibration(state: State<AppState>, device_id: String) -> Option<CalibrationResult> {
    existing_calibration(&state, &device_id).map(|calibration| CalibrationResult {
        device_id: device_id.clone(),
        device_label: calibration
            .device_label
            .unwrap_or_else(|| "已保存设备".into()),
        recommended_threshold: calibration
            .recommended_threshold
            .unwrap_or(calibration.threshold),
        applied_threshold: calibration.threshold,
        noise_floor_db: calibration.noise_floor_db,
        sample_window_ms: calibration.sample_window_ms,
        frame_window_ms: calibration
            .frame_window_ms
            .unwrap_or(state.frame_window_mode().duration_ms()),
        mode: calibration.mode.to_string(),
        updated_at_ms: calibration.updated_at_ms,
        noise_alert: calibration.noise_alert,
        noise_hint: calibration.noise_hint,
        strong_noise_mode: calibration.strong_noise_mode,
    })
}

#[tauri::command]
fn persist_calibration_preference(
    app: AppHandle,
    state: State<AppState>,
    request: PersistCalibrationRequest,
) -> Result<CalibrationResult, String> {
    let mode = request
        .mode
        .parse::<hotkey::CalibrationMode>()
        .map_err(|err| format!("{err}"))?;

    let mut payload = hotkey::SavedCalibration {
        threshold: request.threshold,
        noise_floor_db: request.noise_floor_db,
        sample_window_ms: request.sample_window_ms,
        device_label: request.device_label.clone(),
        mode,
        recommended_threshold: request.recommended_threshold,
        updated_at_ms: None,
        noise_alert: request.noise_alert,
        noise_hint: request.noise_hint.clone(),
        strong_noise_mode: request.strong_noise_mode,
        frame_window_ms: request
            .frame_window_ms
            .or_else(|| Some(state.frame_window_mode().duration_ms())),
    };

    state
        .save_calibration(&request.device_id, payload.clone())
        .map_err(|err| format!("failed to persist calibration: {err}"))?;

    if payload.device_label.is_none() {
        payload.device_label = request.device_label.clone();
    }

    let persisted = existing_calibration(&state, &request.device_id).unwrap_or(payload);

    let mode_label = match persisted.mode {
        hotkey::CalibrationMode::Auto => "自动",
        hotkey::CalibrationMode::Manual => "手动",
    };

    state.session.transition_and_emit(
        &app,
        "CalibrationPersisted",
        format!("{mode_label}模式阈值 {:.2}", persisted.threshold),
    )?;

    Ok(CalibrationResult {
        device_id: request.device_id,
        device_label: persisted
            .device_label
            .unwrap_or_else(|| "已保存设备".into()),
        recommended_threshold: persisted
            .recommended_threshold
            .unwrap_or(persisted.threshold),
        applied_threshold: persisted.threshold,
        noise_floor_db: persisted.noise_floor_db,
        sample_window_ms: persisted.sample_window_ms,
        frame_window_ms: persisted
            .frame_window_ms
            .unwrap_or(state.frame_window_mode().duration_ms()),
        mode: persisted.mode.to_string(),
        updated_at_ms: persisted.updated_at_ms,
        noise_alert: persisted.noise_alert,
        noise_hint: persisted.noise_hint.clone(),
        strong_noise_mode: persisted.strong_noise_mode,
    })
}

#[tauri::command]
fn record_tutorial_event(
    app: AppHandle,
    state: State<AppState>,
    phase: String,
    detail: Option<String>,
) -> Result<SessionStatus, String> {
    let message = detail.unwrap_or_else(|| "".into());
    state.session.transition_and_emit(&app, phase, message)?;
    state.session.snapshot()
}

#[derive(Debug, Serialize)]
struct HotkeyCaptureResponse {
    combination: String,
    conflict_with: Option<String>,
    reason: Option<String>,
}

#[tauri::command]
fn run_audio_diagnostics(
    app: AppHandle,
    state: State<AppState>,
    device_id: Option<String>,
) -> Result<AudioDiagnostics, String> {
    state.session.transition_and_emit(
        &app,
        "DeviceTest",
        "Running five-second microphone diagnostic",
    )?;
    let report: DeviceTestReport = run_device_check(&app, &state, device_id.as_deref())?;
    state.session.transition_and_emit(
        &app,
        "DeviceTestComplete",
        format!(
            "SNR {:.1} dB, peak {:.1} dBFS | 帧窗口 {}ms",
            report.snr_db, report.peak_dbfs, report.frame_window_ms
        ),
    )?;
    Ok(AudioDiagnostics {
        device_id: report.device_id,
        device_label: report.device_label,
        duration_ms: report.duration_ms,
        sample_rate: report.sample_rate,
        snr_db: report.snr_db,
        peak_dbfs: report.peak_dbfs,
        rms_dbfs: report.rms_dbfs,
        noise_floor_db: report.noise_floor_db,
        noise_alert: report.noise_alert,
        noise_hint: report.noise_hint,
        waveform: report.waveform,
        sample_token: report.sample_token,
        frame_window_ms: report.frame_window_ms,
    })
}

#[tauri::command]
fn load_diagnostic_sample(state: State<AppState>, token: String) -> Result<String, String> {
    let bytes = state
        .load_device_sample(&token)
        .map_err(|err| format!("failed to load device sample: {err}"))?;
    Ok(BASE64.encode(bytes))
}

#[tauri::command]
fn open_microphone_privacy_settings() -> Result<(), String> {
    open_microphone_settings()
}

#[tauri::command]
fn open_accessibility_privacy_settings() -> Result<(), String> {
    open_accessibility_settings()
}

#[tauri::command]
fn persist_selected_microphone(
    state: State<AppState>,
    device_id: Option<String>,
) -> Result<(), String> {
    state
        .persist_selected_microphone(device_id)
        .map_err(|err| format!("failed to persist microphone selection: {err}"))
}

#[tauri::command]
fn get_selected_microphone(state: State<AppState>) -> Option<String> {
    state.selected_microphone()
}

#[tauri::command]
fn get_engine_preference(state: State<AppState>) -> Result<EnginePreference, String> {
    Ok(EnginePreference {
        choice: state.engine_choice(),
        recommended: "hybrid".into(),
        privacy_notice:
            "智能混合模式会根据网络状况在本地与云端间切换，云端转写仅在获得租户授权时启用。".into(),
    })
}

#[tauri::command]
fn persist_engine_preference(
    state: State<AppState>,
    choice: String,
) -> Result<EnginePreference, String> {
    state.update_engine_choice(&choice)?;
    get_engine_preference(state)
}

#[tauri::command]
fn skip_tutorial(app: AppHandle, state: State<AppState>) -> Result<SessionStatus, String> {
    state
        .mark_tutorial_skipped()
        .map_err(|err| format!("failed to persist tutorial skip: {err}"))?;
    state.session.transition_and_emit(
        &app,
        "TutorialSkipped",
        "Tutorial deferred for later review",
    )?;
    state.session.snapshot()
}

#[tauri::command]
fn tutorial_completion(state: State<AppState>) -> Result<TutorialCompletionSummary, String> {
    let status = state.tutorial_status();
    Ok(TutorialCompletionSummary {
        finished: state.tutorial_completed(),
        status: status.map(|value| value.to_string()),
    })
}

#[tauri::command]
fn capture_custom_hotkey(
    app: AppHandle,
    state: State<AppState>,
) -> Result<HotkeyCaptureResponse, String> {
    let combination = HotkeyCompatibilityLayer::capture_custom(Duration::from_secs(5))?;
    let conflict = HotkeyCompatibilityLayer::detect_conflict(&app, &combination)?;
    let reason = state.hotkey.lock().ok().and_then(|guard| {
        guard
            .last_probe
            .as_ref()
            .and_then(|probe| probe.reason.clone())
    });

    state.session.transition_and_emit(
        &app,
        "Fallback",
        format!("Captured fallback combination: {combination}"),
    )?;

    Ok(HotkeyCaptureResponse {
        combination,
        conflict_with: conflict,
        reason,
    })
}

#[tauri::command]
fn get_hotkey_binding(state: State<AppState>) -> Result<HotkeyBinding, String> {
    state
        .hotkey
        .lock()
        .map(|guard| guard.binding.clone())
        .map_err(|err| format!("failed to read hotkey binding: {err}"))
}

#[tauri::command]
fn persist_hotkey_binding(
    app: AppHandle,
    state: State<AppState>,
    request: PersistHotkeyRequest,
) -> Result<HotkeyBinding, String> {
    if request.combination.trim().is_empty() {
        return Err("热键组合不能为空".into());
    }

    if let Some(conflict) = HotkeyCompatibilityLayer::detect_conflict(&app, &request.combination)? {
        return Err(format!("组合与系统快捷键 {conflict} 冲突"));
    }

    let mut binding_guard = state
        .hotkey
        .lock()
        .map_err(|err| format!("failed to update hotkey binding: {err}"))?;

    let mut reason = request.reason.clone();
    if request.source == HotkeySource::Custom {
        if reason.is_none() {
            reason = binding_guard
                .last_probe
                .as_ref()
                .and_then(|probe| probe.reason.clone());
        }
        if reason.is_none() {
            reason = Some("Fn 键未被系统捕获，已切换到备用组合。".to_string());
        }
    }

    binding_guard.binding = HotkeyBinding {
        combination: request.combination.clone(),
        source: request.source,
        reason: reason.clone(),
    };

    let persisted = binding_guard.binding.clone();
    drop(binding_guard);

    state.persist_binding(&persisted)?;

    update_tray_hotkey(&app, &persisted);
    state.session.transition_and_emit(
        &app,
        "HotkeyConfigured",
        format!("Active combination: {}", persisted.combination),
    )?;

    Ok(persisted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn sample_key(byte: u8) -> Vec<u8> {
        vec![byte; 32]
    }

    #[test]
    fn sign_and_verify_envelope_roundtrip() {
        let key = sample_key(1);
        let payload = HotkeyConfigPayload {
            combination: "Ctrl+Shift+F".into(),
            source: HotkeySource::Custom,
            reason: Some("fallback".into()),
        };
        let signature = sign_payload(&key, &payload).expect("signing should succeed");
        let envelope = HotkeyConfigEnvelope {
            payload: payload.clone(),
            signature,
        };

        let binding = verify_envelope(&key, envelope).expect("verification should pass");

        assert_eq!(binding.combination, payload.combination);
        assert_eq!(binding.source, payload.source);
        assert_eq!(binding.reason, payload.reason);
    }

    #[test]
    fn verify_envelope_detects_tampering() {
        let key = sample_key(2);
        let payload = HotkeyConfigPayload {
            combination: "Fn".into(),
            source: HotkeySource::Fn,
            reason: None,
        };
        let signature = sign_payload(&key, &payload).expect("signing should succeed");
        let mut envelope = HotkeyConfigEnvelope { payload, signature };
        envelope.payload.combination = "Ctrl+Alt+F".into();

        let result = verify_envelope(&key, envelope);
        assert!(result.is_err(), "tampered envelope must be rejected");
    }

    #[test]
    fn persist_binding_writes_signed_config() {
        let temp = tempdir().expect("tempdir");
        let config_path = temp.path().join("hotkey.json");
        let key = sample_key(3);
        let state = AppState::new(config_path.clone(), key.clone(), HotkeyBinding::default());
        let binding = HotkeyBinding {
            combination: "Ctrl+Alt+Space".into(),
            source: HotkeySource::Custom,
            reason: Some("User opted for fallback".into()),
        };

        state
            .persist_binding(&binding)
            .expect("persisting binding should succeed");

        let raw = fs::read(&config_path).expect("config file should exist");
        let envelope: HotkeyConfigEnvelope =
            serde_json::from_slice(&raw).expect("config should be valid JSON");
        assert_eq!(envelope.payload.combination, binding.combination);
        let verified = verify_envelope(&key, envelope).expect("signature should verify");
        assert_eq!(verified.reason, binding.reason);
    }

    #[test]
    fn append_probe_log_retains_latest_entries() {
        let temp = tempdir().expect("tempdir");
        let config_path = temp.path().join("hotkey.json");
        let key = sample_key(4);
        let state = AppState::new(config_path, key, HotkeyBinding::default());

        for idx in 0..25u128 {
            let probe = FnProbeResult {
                supported: true,
                latency_ms: Some(idx),
                raw_latency_ns: Some(idx * 1_000_000),
                user_reaction_ms: Some(idx + 1),
                within_sla: Some(idx % 2 == 0),
                interface: Some("test".into()),
                device_origin: Some("keyboard".into()),
                reason: Some(format!("entry-{idx}")),
            };
            state
                .append_probe_log(&probe)
                .expect("probe log persistence should succeed");
        }

        let log_path = state.probe_log_path.clone();
        let raw = fs::read(log_path).expect("probe log should exist");
        let entries: Vec<FnProbeLogEntry> =
            serde_json::from_slice(&raw).expect("log should deserialize");
        assert_eq!(entries.len(), 20, "log keeps only the latest 20 entries");
        assert_eq!(entries.first().unwrap().latency_ms, Some(5));
        assert_eq!(entries.last().unwrap().latency_ms, Some(24));
        assert!(entries.iter().all(|entry| entry.reason.is_some()));
    }

    #[test]
    fn load_hotkey_config_roundtrip_and_rejects_invalid_signature() {
        let temp = tempdir().expect("tempdir");
        let config_path = temp.path().join("hotkey.json");
        let key = sample_key(5);
        let payload = HotkeyConfigPayload {
            combination: "Fn".into(),
            source: HotkeySource::Fn,
            reason: None,
        };
        let envelope = HotkeyConfigEnvelope {
            signature: sign_payload(&key, &payload).expect("sign"),
            payload: payload.clone(),
        };
        let serialized = serde_json::to_vec_pretty(&envelope).expect("serialize");
        fs::write(&config_path, serialized).expect("write config");

        let loaded = load_hotkey_config(&config_path, &key).expect("config should load");
        assert_eq!(loaded.combination, payload.combination);

        let mut tampered = envelope;
        tampered.signature = "invalid".into();
        let serialized = serde_json::to_vec_pretty(&tampered).expect("serialize");
        fs::write(&config_path, serialized).expect("write tampered config");

        assert!(
            load_hotkey_config(&config_path, &key).is_none(),
            "invalid signature should be rejected"
        );
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    #[test]
    fn load_or_create_hmac_key_persists_secret() {
        let temp = tempdir().expect("tempdir");
        let config_path = temp.path().join("config").join("hotkey.json");

        let first = load_or_create_hmac_key(&config_path).expect("create key");
        assert_eq!(first.len(), 32);

        let second = load_or_create_hmac_key(&config_path).expect("reuse key");
        assert_eq!(first, second, "subsequent loads reuse the same secret");

        let key_path = config_path.parent().unwrap().join("hotkey.key");
        assert!(key_path.exists(), "secret file should be persisted");
    }

    #[test]
    fn conflicts_list_includes_reserved_shortcuts() {
        let conflicts = HotkeyCompatibilityLayer::conflicts();
        assert!(conflicts.contains(&"Alt+F4".to_string()));
        assert!(conflicts.iter().all(|item| !item.is_empty()));
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    #[test]
    fn probe_fn_reports_platform_gap() {
        let result = HotkeyCompatibilityLayer::probe_fn();
        assert!(!result.supported);
        let reason = result.reason.expect("reason should exist");
        assert!(reason.contains("尚未实现"));
        assert!(result.interface.is_none());
        assert!(result.within_sla.is_none());
    }
}

fn main() {
    tauri::Builder::default()
        .system_tray(build_tray())
        .invoke_handler(tauri::generate_handler![
            session_status,
            session_timeline,
            session_transcript_log,
            session_publish_update,
            session_publish_result,
            session_publish_notice,
            session_publish_history,
            session_publish_results,
            session_publish_notices,
            session_notice_center_history,
            session_history_search,
            session_history_entry,
            session_history_mark_accuracy,
            session_history_append_action,
            session_transcript_apply_selection,
            prime_session_preroll,
            mark_session_processing,
            complete_session_bootstrap,
            start_fn_probe,
            validate_custom_hotkey,
            list_hotkey_conflicts,
            request_microphone_permission,
            request_accessibility_permission,
            check_accessibility_permission,
            permission_status,
            list_audio_inputs,
            run_audio_diagnostics,
            load_diagnostic_sample,
            calibrate_noise_floor,
            get_device_calibration,
            persist_calibration_preference,
            open_microphone_privacy_settings,
            open_accessibility_privacy_settings,
            persist_selected_microphone,
            get_selected_microphone,
            get_engine_preference,
            persist_engine_preference,
            skip_tutorial,
            tutorial_completion,
            record_tutorial_event,
            capture_custom_hotkey,
            get_hotkey_binding,
            persist_hotkey_binding
        ])
        .setup(|app| {
            let config_path = resolve_config_path(app)?;
            let hmac_key = load_or_create_hmac_key(&config_path)?;
            let initial_binding = load_hotkey_config(&config_path, &hmac_key).unwrap_or_default();
            app.manage(AppState::new(
                config_path.clone(),
                hmac_key,
                initial_binding.clone(),
            ));
            update_tray_hotkey(app, &initial_binding);
            let window = app.get_window("main").expect("main window should exist");
            window.set_title("Flowwisper Fn").ok();
            Ok(())
        })
        .on_system_tray_event(|app, event| {
            if let SystemTrayEvent::MenuItemClick { id, .. } = event {
                if id.as_str() == "quit" {
                    app.exit(0);
                }
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running Flowwisper desktop shell");
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PersistCalibrationRequest {
    device_id: String,
    device_label: Option<String>,
    threshold: f32,
    recommended_threshold: Option<f32>,
    noise_floor_db: f32,
    sample_window_ms: u32,
    #[serde(default)]
    frame_window_ms: Option<u32>,
    mode: String,
    #[serde(default)]
    noise_alert: bool,
    #[serde(default)]
    noise_hint: Option<String>,
    #[serde(default)]
    strong_noise_mode: bool,
}
