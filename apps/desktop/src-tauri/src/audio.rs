use crate::hotkey::{AppState, CalibrationMode, SavedCalibration};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, StreamConfig};
use hound::{SampleFormat as WavSampleFormat, WavSpec, WavWriter};
use nnnoiseless::DenoiseState;
use serde::Serialize;
use std::cmp::Ordering;
use std::f32;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};
use tauri::{AppHandle, Emitter};

const NOISE_FLOOR_LIMIT_DB: f32 = -40.0;
const MIN_SNR_DB: f32 = 10.0;
const VAD_ACTIVATE_THRESHOLD: f32 = 0.035;
const VAD_DEACTIVATE_THRESHOLD: f32 = 0.02;
const TARGET_METER_FPS: f32 = 45.0;
const TARGET_SAMPLE_RATE: u32 = 16_000;
const FALLBACK_SAMPLE_RATES: [u32; 2] = [48_000, 44_100];
const DEFAULT_FRAME_WINDOW_MS: u32 = 200;
const FALLBACK_FRAME_WINDOW_MS: u32 = 100;
const SUBFRAME_MS: u32 = 10;
const AGC_TARGET_RMS: f32 = 0.2;
const AGC_MAX_GAIN: f32 = 10.0;
const AGC_MIN_GAIN: f32 = 0.1;
const AGC_ATTACK: f32 = 0.2;
const AGC_RELEASE: f32 = 0.05;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PermissionCheck {
    pub granted: bool,
    pub manual_hint: Option<String>,
    pub detail: Option<String>,
}

#[cfg(any(test, target_os = "windows"))]
#[derive(Debug, Clone, PartialEq, Eq)]
struct WindowsAccessibilityFlow {
    initial_present: bool,
    post_present: bool,
    script_invoked: bool,
    script_error: Option<String>,
}

#[cfg(any(test, target_os = "windows"))]
fn compose_windows_accessibility_response(flow: WindowsAccessibilityFlow) -> PermissionCheck {
    const WINDOWS_MANUAL_HINT: &str =
        "请在 设置 → 无障碍 → 键盘 中启用 Flowwisper，并在提示失败时点击“修复助手”重新注册。";

    if flow.initial_present || flow.post_present {
        return PermissionCheck {
            granted: true,
            manual_hint: None,
            detail: Some("已在 Windows 辅助技术白名单中注册 Flowwisper，可捕获 Fn 按键。".into()),
        };
    }

    if flow.script_invoked {
        return PermissionCheck {
            granted: false,
            manual_hint: Some(WINDOWS_MANUAL_HINT.into()),
            detail: Some(flow.script_error.unwrap_or_else(|| {
                "尝试注册辅助功能后仍未检测到 Flowwisper，请检查系统设置。".into()
            })),
        };
    }

    PermissionCheck {
        granted: false,
        manual_hint: Some(WINDOWS_MANUAL_HINT.into()),
        detail: Some("尚未在 Windows 辅助技术白名单中检测到 Flowwisper。".into()),
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct DeviceSummary {
    pub id: String,
    pub label: String,
    pub kind: String,
    pub preferred: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct DeviceTestReport {
    pub device_id: String,
    pub device_label: String,
    pub duration_ms: u32,
    pub sample_rate: u32,
    pub snr_db: f32,
    pub peak_dbfs: f32,
    pub rms_dbfs: f32,
    pub noise_floor_db: f32,
    pub noise_alert: bool,
    pub noise_hint: Option<String>,
    pub waveform: Vec<f32>,
    pub sample_token: String,
    pub frame_window_ms: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct AudioMeterFrame {
    pub context: String,
    pub device_id: String,
    pub peak: f32,
    pub rms: f32,
    pub vad_active: bool,
    pub timestamp_ms: u128,
}

#[derive(Debug, Clone)]
pub struct CalibrationComputation {
    pub device_id: String,
    pub device_label: String,
    pub recommended_threshold: f32,
    pub noise_floor_db: f32,
    pub sample_window_ms: u32,
    pub frame_window_ms: u32,
}

struct CapturedAudio {
    samples: Vec<f32>,
    sample_rate: u32,
    duration_ms: u32,
    frame_window_ms: u32,
}

struct NoiseSuppressor {
    state: Option<Box<DenoiseState<'static>>>,
    frame_size: usize,
    upsample_factor: usize,
}

impl NoiseSuppressor {
    fn new(frame_size: usize) -> Self {
        let denoise_frame = DenoiseState::FRAME_SIZE;
        let upsample_factor = if frame_size > 0 && denoise_frame % frame_size == 0 {
            denoise_frame / frame_size
        } else {
            0
        };
        let state = if upsample_factor > 0 {
            Some(DenoiseState::new())
        } else {
            None
        };
        Self {
            state,
            frame_size,
            upsample_factor,
        }
    }

    fn process(&mut self, frame: &[f32]) -> Vec<f32> {
        if self.state.is_none() || self.upsample_factor == 0 || frame.is_empty() {
            return frame.to_vec();
        }

        let mut upsampled = upsample_linear(frame, self.upsample_factor);
        if upsampled.len() != DenoiseState::FRAME_SIZE {
            let pad_value = *upsampled.last().unwrap_or(&0.0);
            upsampled.resize(DenoiseState::FRAME_SIZE, pad_value);
        }

        let mut input = vec![0.0f32; DenoiseState::FRAME_SIZE];
        let mut output = vec![0.0f32; DenoiseState::FRAME_SIZE];
        for (idx, value) in upsampled.iter().enumerate() {
            input[idx] = (value * 32_768.0).clamp(-32_768.0, 32_767.0);
        }

        if let Some(state) = self.state.as_mut() {
            state.process_frame(&mut output[..], &input[..]);
        } else {
            output.copy_from_slice(&input[..]);
        }

        downsample_average(&output, self.upsample_factor, self.frame_size)
    }
}

fn upsample_linear(frame: &[f32], factor: usize) -> Vec<f32> {
    if frame.is_empty() || factor == 0 {
        return Vec::new();
    }

    let mut upsampled = Vec::with_capacity(frame.len() * factor);
    for window in frame.windows(2) {
        let start = window[0];
        let end = window[1];
        for sub in 0..factor {
            let t = sub as f32 / factor as f32;
            upsampled.push(start + (end - start) * t);
        }
    }

    let last = *frame.last().unwrap();
    for _ in 0..factor {
        upsampled.push(last);
    }
    upsampled
}

fn downsample_average(samples: &[f32], factor: usize, target_len: usize) -> Vec<f32> {
    if samples.is_empty() || factor == 0 || target_len == 0 {
        return Vec::new();
    }

    let mut downsampled = Vec::with_capacity(target_len);
    for idx in 0..target_len {
        let start = idx * factor;
        let end = (start + factor).min(samples.len());
        if start >= end {
            downsampled.push(0.0);
            continue;
        }
        let acc: f32 = samples[start..end].iter().sum();
        downsampled.push(acc / (end - start) as f32);
    }
    downsampled
}

struct AutomaticGainControl {
    target_rms: f32,
    max_gain: f32,
    min_gain: f32,
    attack: f32,
    release: f32,
    current_gain: f32,
}

impl AutomaticGainControl {
    fn new() -> Self {
        Self {
            target_rms: AGC_TARGET_RMS,
            max_gain: AGC_MAX_GAIN,
            min_gain: AGC_MIN_GAIN,
            attack: AGC_ATTACK,
            release: AGC_RELEASE,
            current_gain: 1.0,
        }
    }

    fn process_frame(&mut self, frame: &[f32]) -> Vec<f32> {
        if frame.is_empty() {
            return Vec::new();
        }
        let rms = (frame.iter().map(|sample| sample * sample).sum::<f32>() / frame.len() as f32)
            .sqrt()
            .max(1e-6);
        let mut desired_gain = self.target_rms / rms;
        desired_gain = desired_gain.clamp(self.min_gain, self.max_gain);
        let smoothing = if desired_gain > self.current_gain {
            self.attack
        } else {
            self.release
        };
        self.current_gain += (desired_gain - self.current_gain) * smoothing;
        frame
            .iter()
            .map(|sample| (sample * self.current_gain).clamp(-1.0, 1.0))
            .collect()
    }
}

struct SampleRateConverter {
    from_rate: u32,
    to_rate: u32,
    step: f64,
    cursor: f64,
    buffer: Vec<f32>,
}

impl SampleRateConverter {
    fn new(from_rate: u32, to_rate: u32) -> Self {
        let step = if to_rate == 0 {
            1.0
        } else {
            from_rate as f64 / to_rate as f64
        };
        Self {
            from_rate,
            to_rate,
            step,
            cursor: 0.0,
            buffer: Vec::new(),
        }
    }

    fn convert_block(&mut self, input: &[f32]) -> Vec<f32> {
        if self.from_rate == 0 || self.to_rate == 0 || self.from_rate == self.to_rate {
            return input.to_vec();
        }
        if input.is_empty() {
            return Vec::new();
        }

        self.buffer.extend_from_slice(input);
        if self.buffer.len() < 2 {
            return Vec::new();
        }

        let mut output = Vec::new();
        let mut cursor = self.cursor;
        while cursor + 1.0 < self.buffer.len() as f64 {
            let base = cursor.floor() as usize;
            let frac = (cursor - base as f64) as f32;
            let current = self.buffer[base];
            let next = self.buffer[base + 1];
            output.push(current + (next - current) * frac);
            cursor += self.step;
        }

        let consumed_floor = cursor.floor() as usize;
        let consumed = if consumed_floor > 1 {
            consumed_floor - 1
        } else {
            0
        };
        if consumed > 0 && consumed <= self.buffer.len() {
            self.buffer.drain(0..consumed);
            cursor -= consumed as f64;
        }

        self.cursor = cursor;
        output
    }

    fn flush(&mut self) -> Vec<f32> {
        if self.from_rate == 0 || self.to_rate == 0 || self.from_rate == self.to_rate {
            let remaining = self.buffer.clone();
            self.buffer.clear();
            self.cursor = 0.0;
            return remaining;
        }

        if self.buffer.is_empty() {
            self.cursor = 0.0;
            return Vec::new();
        }

        let last = *self.buffer.last().unwrap();
        self.buffer.push(last);

        let mut output = Vec::new();
        let mut cursor = self.cursor;
        while cursor + 1.0 < self.buffer.len() as f64 {
            let base = cursor.floor() as usize;
            let frac = (cursor - base as f64) as f32;
            let current = self.buffer[base];
            let next = self.buffer[base + 1];
            output.push(current + (next - current) * frac);
            cursor += self.step;
        }

        self.buffer.clear();
        self.cursor = 0.0;
        output
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameWindowSetting {
    Default,
    Fallback,
}

impl Default for FrameWindowSetting {
    fn default() -> Self {
        FrameWindowSetting::Default
    }
}

impl FrameWindowSetting {
    pub fn duration_ms(self) -> u32 {
        match self {
            FrameWindowSetting::Default => DEFAULT_FRAME_WINDOW_MS,
            FrameWindowSetting::Fallback => FALLBACK_FRAME_WINDOW_MS,
        }
    }

    fn samples(self) -> usize {
        ((TARGET_SAMPLE_RATE as usize) * self.duration_ms() as usize) / 1000
    }
}

struct DspProcessor {
    subframe_size: usize,
    frame_window: FrameWindowSetting,
    frame_window_samples: usize,
    pending_subframe: Vec<f32>,
    pending_window: Vec<f32>,
    suppressor: NoiseSuppressor,
    agc: AutomaticGainControl,
    resampler: SampleRateConverter,
}

impl DspProcessor {
    fn new(sample_rate: u32, frame_window: FrameWindowSetting) -> Self {
        let subframe_size = ((TARGET_SAMPLE_RATE as usize) / (1000 / SUBFRAME_MS as usize)).max(1);
        let frame_window_samples = frame_window.samples().max(1);
        Self {
            subframe_size,
            frame_window,
            frame_window_samples,
            pending_subframe: Vec::with_capacity(subframe_size),
            pending_window: Vec::with_capacity(frame_window_samples),
            suppressor: NoiseSuppressor::new(subframe_size),
            agc: AutomaticGainControl::new(),
            resampler: SampleRateConverter::new(sample_rate, TARGET_SAMPLE_RATE),
        }
    }

    fn process_block(&mut self, input: &[f32]) -> Vec<f32> {
        if input.is_empty() {
            return Vec::new();
        }

        let resampled = self.resampler.convert_block(input);
        self.consume_resampled(&resampled)
    }

    fn flush(&mut self) -> Vec<f32> {
        let mut output = Vec::new();
        let leftover = self.resampler.flush();
        if !leftover.is_empty() {
            output.extend(self.consume_resampled(&leftover));
        }

        if !self.pending_subframe.is_empty() {
            let pad_value = *self.pending_subframe.last().unwrap_or(&0.0);
            let mut frame = self.pending_subframe.clone();
            while frame.len() < self.subframe_size {
                frame.push(pad_value);
            }
            let processed = self.process_subframe(&frame);
            self.pending_subframe.clear();
            self.pending_window
                .extend_from_slice(&processed[..self.subframe_size]);
        }

        if !self.pending_window.is_empty() {
            output.extend_from_slice(&self.pending_window);
            self.pending_window.clear();
        }

        output
    }

    fn process_subframe(&mut self, frame: &[f32]) -> Vec<f32> {
        let denoised = self.suppressor.process(frame);
        self.agc.process_frame(&denoised)
    }

    fn consume_resampled(&mut self, resampled: &[f32]) -> Vec<f32> {
        if resampled.is_empty() {
            return Vec::new();
        }

        let mut output = Vec::new();
        let mut index = 0;
        while index < resampled.len() {
            let remaining = self.subframe_size - self.pending_subframe.len();
            let take = remaining.min(resampled.len() - index);
            self.pending_subframe
                .extend_from_slice(&resampled[index..index + take]);
            index += take;

            if self.pending_subframe.len() == self.subframe_size {
                let frame = self.pending_subframe.clone();
                self.pending_subframe.clear();
                let processed = self.process_subframe(&frame);
                self.pending_window.extend_from_slice(&processed);
                while self.pending_window.len() >= self.frame_window_samples {
                    output.extend_from_slice(&self.pending_window[..self.frame_window_samples]);
                    self.pending_window.drain(..self.frame_window_samples);
                }
            }
        }
        output
    }
}

pub fn request_microphone_permission() -> Result<PermissionCheck, String> {
    #[cfg(target_os = "macos")]
    {
        return request_microphone_permission_macos();
    }

    #[cfg(target_os = "windows")]
    {
        return request_microphone_permission_windows();
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        Ok(PermissionCheck {
            granted: true,
            manual_hint: None,
            detail: Some("Linux 不需要额外的麦克风授权".into()),
        })
    }
}

pub fn open_microphone_settings() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_Microphone")
            .status()
            .map_err(|err| format!("无法打开系统设置: {err}"))?;
        return Ok(());
    }

    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("cmd")
            .args(["/C", "start", "ms-settings:privacy-microphone"])
            .status()
            .map_err(|err| format!("无法打开系统麦克风设置: {err}"))?;
        return Ok(());
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        Err("当前平台无需额外的麦克风设置".into())
    }
}

pub fn request_accessibility_permission() -> Result<PermissionCheck, String> {
    ensure_accessibility_permission(true)
}

pub fn check_accessibility_permission() -> Result<PermissionCheck, String> {
    ensure_accessibility_permission(false)
}

fn ensure_accessibility_permission(prompt: bool) -> Result<PermissionCheck, String> {
    #[cfg(target_os = "macos")]
    {
        return request_accessibility_permission_macos(prompt);
    }

    #[cfg(target_os = "windows")]
    {
        return request_accessibility_permission_windows(prompt);
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        Ok(PermissionCheck {
            granted: true,
            manual_hint: None,
            detail: Some("当前平台无需辅助功能权限".into()),
        })
    }
}

pub fn open_accessibility_settings() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility")
            .status()
            .map_err(|err| format!("无法打开系统辅助功能设置: {err}"))?;
        return Ok(());
    }

    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("cmd")
            .args(["/C", "start", "ms-settings:easeofaccess-keyboard"])
            .status()
            .map_err(|err| format!("无法打开 Windows 辅助功能设置: {err}"))?;
        return Ok(());
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        Err("当前平台无需辅助功能权限".into())
    }
}

#[cfg(target_os = "windows")]
fn request_accessibility_permission_windows(prompt: bool) -> Result<PermissionCheck, String> {
    let initial_present = windows_accessibility::is_registered()?;
    if initial_present {
        return Ok(compose_windows_accessibility_response(
            WindowsAccessibilityFlow {
                initial_present,
                post_present: true,
                script_invoked: false,
                script_error: None,
            },
        ));
    }

    if prompt {
        let exe_path = std::env::current_exe()
            .map_err(|err| format!("无法获取应用路径用于注册辅助功能: {err}"))?;
        match windows_accessibility::register_bridge(&exe_path) {
            Ok(()) => {
                let post_present = windows_accessibility::is_registered()?;
                return Ok(compose_windows_accessibility_response(
                    WindowsAccessibilityFlow {
                        initial_present,
                        post_present,
                        script_invoked: true,
                        script_error: None,
                    },
                ));
            }
            Err(err) => {
                return Ok(compose_windows_accessibility_response(
                    WindowsAccessibilityFlow {
                        initial_present,
                        post_present: false,
                        script_invoked: true,
                        script_error: Some(err),
                    },
                ));
            }
        }
    }

    Ok(compose_windows_accessibility_response(
        WindowsAccessibilityFlow {
            initial_present,
            post_present: false,
            script_invoked: false,
            script_error: None,
        },
    ))
}

#[cfg(target_os = "windows")]
mod windows_accessibility {
    use std::path::Path;
    use std::process::Command;
    use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS};
    use windows::Win32::System::Registry::{
        RegCloseKey, RegOpenKeyExW, HKEY, HKEY_CURRENT_USER, KEY_READ,
    };

    const REGISTRY_PATH: &str =
        "Software\\Microsoft\\Windows NT\\CurrentVersion\\Accessibility\\ATs\\Flowwisper";

    pub fn is_registered() -> Result<bool, String> {
        unsafe {
            let mut handle = HKEY::default();
            let status = RegOpenKeyExW(
                HKEY_CURRENT_USER,
                windows::w!(REGISTRY_PATH),
                0,
                KEY_READ,
                &mut handle,
            );
            if status == ERROR_SUCCESS.0 {
                let _ = RegCloseKey(handle);
                Ok(true)
            } else if status == ERROR_FILE_NOT_FOUND.0 {
                Ok(false)
            } else {
                Err(format!("无法读取辅助功能注册表: 错误码 {status}"))
            }
        }
    }

    pub fn register_bridge(exe_path: &Path) -> Result<(), String> {
        let exe = exe_path
            .to_str()
            .ok_or_else(|| "无法解析应用路径字符串".to_string())?
            .replace('"', "\"");
        let script = format!(
            r#"
$base = 'HKCU:\\Software\\Microsoft\\Windows NT\\CurrentVersion\\Accessibility\\ATs'
$path = Join-Path $base 'Flowwisper'
if (-not (Test-Path $path)) {{
  New-Item -Path $path -Force | Out-Null
}}
New-ItemProperty -Path $path -Name 'Description' -Value 'Flowwisper Accessibility Bridge' -Force | Out-Null
New-ItemProperty -Path $path -Name 'ATExe' -Value "{exe}" -Force | Out-Null
New-ItemProperty -Path $path -Name 'ATFriendlyName' -Value 'Flowwisper' -Force | Out-Null
"#
        );

        let status = Command::new("powershell")
            .args([
                "-NoProfile",
                "-ExecutionPolicy",
                "Bypass",
                "-Command",
                &script,
            ])
            .status()
            .map_err(|err| format!("无法执行辅助功能注册脚本: {err}"))?;

        if status.success() {
            Ok(())
        } else {
            Err(format!(
                "PowerShell 注册脚本执行失败，退出码 {:?}",
                status.code()
            ))
        }
    }
}

#[cfg(test)]
mod windows_tests {
    use super::{
        compose_windows_accessibility_response, PermissionCheck, WindowsAccessibilityFlow,
    };

    fn manual_hint(check: &PermissionCheck) -> &str {
        check.manual_hint.as_deref().unwrap_or("提示缺失")
    }

    #[test]
    fn windows_accessibility_reports_granted_when_registered() {
        let result = compose_windows_accessibility_response(WindowsAccessibilityFlow {
            initial_present: true,
            post_present: true,
            script_invoked: false,
            script_error: None,
        });

        assert!(
            result.granted,
            "expected granted when registry already present"
        );
        assert!(
            result
                .detail
                .as_deref()
                .unwrap_or_default()
                .contains("白名单"),
            "detail should mention whitelist registration"
        );
        assert!(result.manual_hint.is_none());
    }

    #[test]
    fn windows_accessibility_requires_registration_when_missing() {
        let result = compose_windows_accessibility_response(WindowsAccessibilityFlow {
            initial_present: false,
            post_present: false,
            script_invoked: false,
            script_error: None,
        });

        assert!(
            !result.granted,
            "permission should be denied when not registered"
        );
        assert!(
            manual_hint(&result).contains("无障碍"),
            "manual hint should direct users to accessibility settings"
        );
        assert!(
            result
                .detail
                .as_deref()
                .unwrap_or_default()
                .contains("尚未在 Windows 辅助技术白名单"),
            "detail should highlight missing registration"
        );
    }

    #[test]
    fn windows_accessibility_surfaces_script_errors() {
        let message = "PowerShell 注册脚本执行失败".to_string();
        let result = compose_windows_accessibility_response(WindowsAccessibilityFlow {
            initial_present: false,
            post_present: false,
            script_invoked: true,
            script_error: Some(message.clone()),
        });

        assert!(
            manual_hint(&result).contains("修复助手"),
            "manual hint should nudge users toward recovery actions"
        );
        assert_eq!(result.detail.as_deref(), Some(message.as_str()));
        assert!(!result.granted);
    }
}

pub fn list_devices() -> Result<Vec<DeviceSummary>, String> {
    let host = cpal::default_host();
    let default_name = host
        .default_input_device()
        .and_then(|device| device.name().ok());

    let devices = host
        .input_devices()
        .map_err(|err| format!("无法枚举音频输入设备: {err}"))?;

    let mut result = Vec::new();
    for (index, device) in devices.enumerate() {
        let label = device
            .name()
            .unwrap_or_else(|_| format!("输入设备 #{index}"));
        let id = format!("{}::{}", sanitize_identifier(&label), index);
        let kind = classify_device(&label);
        let preferred = default_name
            .as_ref()
            .map(|name| name == &label)
            .unwrap_or(index == 0);
        result.push(DeviceSummary {
            id,
            label,
            kind,
            preferred,
        });
    }

    if result.is_empty() {
        Err("未检测到可用的音频输入设备".into())
    } else {
        Ok(result)
    }
}

pub fn run_device_check(
    app: &AppHandle,
    state: &AppState,
    device_id: Option<&str>,
) -> Result<DeviceTestReport, String> {
    let (device, label) = resolve_device(device_id)?;
    let resolved_id = device_id
        .map(|id| id.to_string())
        .unwrap_or_else(|| format!("{}::default", sanitize_identifier(&label)));
    let capture = capture_audio(
        &device,
        Duration::from_secs(5),
        Some(MeterContext::new(
            app.clone(),
            "device-test".into(),
            resolved_id.clone(),
        )),
        state.frame_window_mode(),
    )?;
    let analytics = analyze_samples(&capture.samples);
    let (noise_alert, noise_hint) = assess_noise(&analytics);
    let sample_token = format!(
        "device-test-{}-{}",
        sanitize_identifier(&label),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map_err(|err| format!("system time error: {err}"))?
            .as_millis()
    );
    let wav_bytes = encode_wave(&capture.samples, capture.sample_rate)?;
    state
        .store_device_sample(&sample_token, &wav_bytes)
        .map_err(|err| format!("无法保存诊断样本: {err}"))?;

    let waveform = summarise_waveform(&capture.samples, 180);

    Ok(DeviceTestReport {
        device_id: resolved_id,
        device_label: label,
        duration_ms: capture.duration_ms,
        sample_rate: capture.sample_rate,
        snr_db: analytics.snr_db,
        peak_dbfs: analytics.peak_db,
        rms_dbfs: analytics.rms_db,
        noise_floor_db: analytics.noise_floor_db,
        noise_alert,
        noise_hint: noise_hint.clone(),
        waveform,
        sample_token,
        frame_window_ms: capture.frame_window_ms,
    })
}

pub fn prime_waveform_bridge(
    app: AppHandle,
    device_id: Option<String>,
    duration: Duration,
    frame_window: FrameWindowSetting,
) -> Result<(), String> {
    let (device, label) = resolve_device(device_id.as_deref())?;
    let resolved_id =
        device_id.unwrap_or_else(|| format!("{}::default", sanitize_identifier(&label)));
    let _ = capture_audio(
        &device,
        duration,
        Some(MeterContext::new(app, "fn-preroll".into(), resolved_id)),
        frame_window,
    )?;
    Ok(())
}

pub fn calibrate_device(
    device_id: Option<&str>,
    state: &AppState,
) -> Result<(CalibrationComputation, SavedCalibration), String> {
    let (device, label) = resolve_device(device_id)?;
    let capture = capture_audio(
        &device,
        Duration::from_secs(5),
        None,
        state.frame_window_mode(),
    )?;
    let analytics = analyze_samples(&capture.samples);
    let recommended_threshold = ((analytics.snr_db + 10.0) / 80.0).clamp(0.2, 0.9);
    let device_key = device_id
        .map(|id| id.to_string())
        .unwrap_or_else(|| format!("{}::default", sanitize_identifier(&label)));

    let (noise_alert, noise_hint) = assess_noise(&analytics);
    let existing = state.calibration_for(&device_key);
    let strong_noise_mode = existing
        .as_ref()
        .map(|calibration| calibration.strong_noise_mode)
        .unwrap_or(false);

    let saved = SavedCalibration {
        threshold: recommended_threshold,
        noise_floor_db: analytics.noise_floor_db,
        sample_window_ms: capture.duration_ms,
        device_label: Some(label.clone()),
        mode: CalibrationMode::Auto,
        recommended_threshold: Some(recommended_threshold),
        updated_at_ms: None,
        noise_alert,
        noise_hint: noise_hint.clone(),
        strong_noise_mode,
        frame_window_ms: Some(capture.frame_window_ms),
    };

    let computation = CalibrationComputation {
        device_id: device_key.clone(),
        device_label: label,
        recommended_threshold,
        noise_floor_db: analytics.noise_floor_db,
        sample_window_ms: capture.duration_ms,
        frame_window_ms: capture.frame_window_ms,
    };

    state.save_calibration(&device_key, saved.clone())?;
    Ok((computation, saved))
}

pub fn existing_calibration(state: &AppState, device_id: &str) -> Option<SavedCalibration> {
    state.calibration_for(device_id)
}

fn request_microphone_permission_macos() -> Result<PermissionCheck, String> {
    use block::ConcreteBlock;
    use objc::runtime::Object;
    use objc::{class, msg_send, sel, sel_impl};
    use std::sync::mpsc;
    use std::time::Duration as StdDuration;
    use tauri::async_runtime::block_on;

    // 在主线程上执行 UI 相关的操作以避免崩溃
    block_on(async move {
        unsafe {
            // 尝试获取 AVAudioSession
            let session_cls = class!(AVAudioSession);
            let session: *mut Object = msg_send![session_cls, sharedInstance];
            if session.is_null() {
                return Err("无法获取 AVAudioSession 实例".into());
            }
            
            // 检查 session 是否响应 requestRecordPermission: 方法
            let sel = sel!(requestRecordPermission:);
            let responds: bool = msg_send![session, respondsToSelector: sel];
            if !responds {
                return Err("AVAudioSession 不响应 requestRecordPermission: 方法".into());
            }
            
            // 检查应用是否已激活
            let app_cls = class!(NSApplication);
            let app: *mut Object = msg_send![app_cls, sharedApplication];
            if !app.is_null() {
                let activation_policy: i32 = msg_send![app, activationPolicy];
                if activation_policy != 0 { // NSApplicationActivationPolicyRegular
                    let _: () = msg_send![app, setActivationPolicy: 0]; // NSApplicationActivationPolicyRegular
                }
                
                // 激活应用
                let _: () = msg_send![app, activateIgnoringOtherApps: true];
            }
            
            let (sender, receiver) = mpsc::channel();
            
            // 创建 block，使用 Arc 来确保生命周期
            let sender = std::sync::Arc::new(std::sync::Mutex::new(Some(sender)));
            let sender_clone = sender.clone();
            
            let block = ConcreteBlock::new(move |granted: bool| {
                if let Ok(mut sender) = sender_clone.lock() {
                    if let Some(sender) = sender.take() {
                        let _ = sender.send(granted);
                    }
                }
            });
            
            // 复制 block 以确保它有正确的引用计数
            let block = block.copy();
            
            // 现在可以安全地调用方法
            let _: () = msg_send![session, requestRecordPermission: &*block];

            match receiver.recv_timeout(StdDuration::from_secs(5)) {
                Ok(true) => Ok(PermissionCheck {
                    granted: true,
                    manual_hint: None,
                    detail: Some("已通过 AVAudioSession 请求麦克风权限".into()),
                }),
                Ok(false) => Ok(PermissionCheck {
                    granted: false,
                    manual_hint: Some(
                        "请前往 系统设置 → 隐私与安全 → 麦克风，手动启用 Flowwisper。".into(),
                    ),
                    detail: Some("用户拒绝了麦克风权限".into()),
                }),
                Err(_) => Err("未能在 5 秒内获取麦克风权限结果".into()),
            }
        }
    })
}

#[cfg(target_os = "windows")]
fn request_microphone_permission_windows() -> Result<PermissionCheck, String> {
    use tauri::async_runtime::block_on;
    use windows::core::Error;
    use windows::Media::Capture::{
        MediaCapture, MediaCaptureInitializationSettings, StreamingCaptureMode,
    };

    block_on(async move {
        let capture = MediaCapture::new()?;
        let settings = MediaCaptureInitializationSettings::new()?;
        settings.SetStreamingCaptureMode(StreamingCaptureMode::Audio)?;
        match capture.InitializeWithSettingsAsync(&settings)?.await {
            Ok(_) => Ok(PermissionCheck {
                granted: true,
                manual_hint: None,
                detail: Some("成功初始化 MediaCapture 音频会话".into()),
            }),
            Err(err) => Err(err),
        }
    })
    .map_err(|err: Error| {
        format!(
            "请求麦克风权限失败: {:?}. 请在 设置 → 隐私与安全 → 麦克风 中开启 Flowwisper。",
            err
        )
    })
    .and_then(|result| {
        if result.granted {
            Ok(result)
        } else {
            Ok(PermissionCheck {
                granted: false,
                manual_hint: Some(
                    "请在 设置 → 隐私与安全 → 麦克风 中启用 Flowwisper，并允许应用访问麦克风。"
                        .into(),
                ),
                detail: result.detail,
            })
        }
    })
}

#[cfg(target_os = "macos")]
fn request_accessibility_permission_macos(prompt: bool) -> Result<PermissionCheck, String> {
    use accessibility_sys::{kAXTrustedCheckOptionPrompt, AXIsProcessTrustedWithOptions};
    use core_foundation::base::TCFType;
    use core_foundation::boolean::CFBoolean;
    use core_foundation::dictionary::CFMutableDictionary;
    use core_foundation::string::CFString;

    unsafe {
        let key = CFString::wrap_under_get_rule(kAXTrustedCheckOptionPrompt);
        let value = CFBoolean::from(prompt);
        let mut options = CFMutableDictionary::<CFString, CFBoolean>::new();
        options.set(key, value);
        let trusted = AXIsProcessTrustedWithOptions(options.to_immutable().as_concrete_TypeRef());
        if trusted {
            Ok(PermissionCheck {
                granted: true,
                manual_hint: None,
                detail: Some("已获得辅助功能权限，可捕获 Fn 键事件".into()),
            })
        } else {
            Ok(PermissionCheck {
                granted: false,
                manual_hint: Some(
                    "请前往 系统设置 → 隐私与安全 → 辅助功能，启用 Flowwisper。".into(),
                ),
                detail: Some("macOS 尚未授予辅助功能权限".into()),
            })
        }
    }
}

fn resolve_device(device_id: Option<&str>) -> Result<(cpal::Device, String), String> {
    let host = cpal::default_host();
    let mut devices = host
        .input_devices()
        .map_err(|err| format!("无法枚举音频输入设备: {err}"))?;

    if let Some(id) = device_id {
        for (index, device) in devices.enumerate() {
            let label = device
                .name()
                .unwrap_or_else(|_| format!("输入设备 #{index}"));
            let candidate_id = format!("{}::{}", sanitize_identifier(&label), index);
            if candidate_id == id {
                return Ok((device, label));
            }
        }
        Err(format!("未找到匹配的设备 {id}"))
    } else if let Some(default_device) = host.default_input_device() {
        let label = default_device
            .name()
            .unwrap_or_else(|_| "默认输入设备".into());
        Ok((default_device, label))
    } else {
        Err("系统没有可用的默认音频输入设备".into())
    }
}

fn capture_audio(
    device: &cpal::Device,
    duration: Duration,
    meter: Option<MeterContext>,
    frame_window: FrameWindowSetting,
) -> Result<CapturedAudio, String> {
    let (stream_config, sample_format) = select_stream_config(device)?;
    let source_sample_rate = stream_config.sample_rate.0;
    let channels = stream_config.channels as usize;
    let collected: Arc<Mutex<Vec<f32>>> = Arc::new(Mutex::new(Vec::new()));
    let target = collected.clone();
    let meter =
        meter.map(|context| Arc::new(Mutex::new(MeterEmitter::new(context, TARGET_SAMPLE_RATE))));
    let dsp = Arc::new(Mutex::new(DspProcessor::new(
        source_sample_rate,
        frame_window,
    )));
    let err_fn = |err| {
        eprintln!("audio capture error: {err}");
    };

    let stream = match sample_format {
        SampleFormat::F32 => {
            let writer = target.clone();
            let meter = meter.clone();
            let processor = dsp.clone();
            device.build_input_stream(
                &stream_config,
                move |data: &[f32], _| {
                    push_samples(data, channels, &writer, meter.as_ref(), Some(&processor));
                },
                err_fn,
                None,
            )
        }
        SampleFormat::I16 => {
            let writer = target.clone();
            let meter = meter.clone();
            let processor = dsp.clone();
            device.build_input_stream(
                &stream_config,
                move |data: &[i16], _| {
                    let converted: Vec<f32> = data
                        .iter()
                        .map(|sample| *sample as f32 / i16::MAX as f32)
                        .collect();
                    push_samples(
                        &converted,
                        channels,
                        &writer,
                        meter.as_ref(),
                        Some(&processor),
                    );
                },
                err_fn,
                None,
            )
        }
        SampleFormat::U16 => {
            let writer = target.clone();
            let meter = meter.clone();
            let processor = dsp.clone();
            device.build_input_stream(
                &stream_config,
                move |data: &[u16], _| {
                    let converted: Vec<f32> = data
                        .iter()
                        .map(|sample| (*sample as f32 / u16::MAX as f32) * 2.0 - 1.0)
                        .collect();
                    push_samples(
                        &converted,
                        channels,
                        &writer,
                        meter.as_ref(),
                        Some(&processor),
                    );
                },
                err_fn,
                None,
            )
        }
        other => {
            return Err(format!(
                "不支持的音频采样格式: {:?}. 请切换到 PCM16/PCM32 设备",
                other
            ));
        }
    }
    .map_err(|err| format!("构建输入流失败: {err}"))?;

    stream
        .play()
        .map_err(|err| format!("启动录音失败: {err}"))?;
    
    // 等待录音完成，但添加超时保护
    std::thread::sleep(duration + Duration::from_millis(120));
    
    // 先暂停流，然后完全停止并清理
    let _ = stream.pause();
    drop(stream);
    
    // 给系统一点时间来清理音频线程
    std::thread::sleep(Duration::from_millis(10));

    let flushed = dsp
        .lock()
        .map(|mut processor| processor.flush())
        .unwrap_or_default();

    if !flushed.is_empty() {
        if let Some(meter) = meter.as_ref() {
            if let Ok(mut emitter) = meter.lock() {
                for value in &flushed {
                    emitter.push(value.abs());
                }
            }
        }
        if let Ok(mut buffer) = target.lock() {
            buffer.extend(flushed.iter().copied());
        }
    }

    if let Some(meter) = meter.as_ref() {
        if let Ok(mut emitter) = meter.lock() {
            emitter.flush();
        }
        // 给计量器一点时间来完成最后的处理
        std::thread::sleep(Duration::from_millis(5));
    }

    drop(target);

    // 尝试获取数据，但如果Arc还有引用，则使用克隆方式
    let samples = match Arc::try_unwrap(collected) {
        Ok(arc) => arc.into_inner().map_err(|err| format!("无法锁定录音缓冲区: {err}"))?,
        Err(arc) => {
            // 如果无法unwrap，从Arc中克隆数据
            arc.lock().map_err(|err| format!("无法锁定录音缓冲区: {err}"))?.clone()
        }
    };

    if samples.is_empty() {
        Err("录音缓冲为空，请确认麦克风是否可用".into())
    } else {
        let duration_ms = ((samples.len() as f64 / TARGET_SAMPLE_RATE as f64) * 1000.0) as u32;
        Ok(CapturedAudio {
            samples,
            sample_rate: TARGET_SAMPLE_RATE,
            duration_ms,
            frame_window_ms: frame_window.duration_ms(),
        })
    }
}

fn select_stream_config(device: &cpal::Device) -> Result<(StreamConfig, SampleFormat), String> {
    let supported_configs: Vec<_> = device
        .supported_input_configs()
        .map_err(|err| format!("无法枚举音频输入配置: {err}"))?
        .collect();

    for &rate in std::iter::once(&TARGET_SAMPLE_RATE).chain(FALLBACK_SAMPLE_RATES.iter()) {
        if let Some(config) = supported_configs.iter().find_map(|range| {
            let min_rate = range.min_sample_rate().0;
            let max_rate = range.max_sample_rate().0;
            if (min_rate..=max_rate).contains(&rate) {
                let supported = range.with_sample_rate(cpal::SampleRate(rate));
                let sample_format = supported.sample_format();
                let mut stream_config = supported.config().clone();
                stream_config.sample_rate = cpal::SampleRate(rate);
                Some((stream_config, sample_format))
            } else {
                None
            }
        }) {
            return Ok(config);
        }
    }

    let mut best_config = None;
    for range in &supported_configs {
        let supported = range.with_max_sample_rate();
        let sample_format = supported.sample_format();
        let mut stream_config = supported.config().clone();
        let rate = stream_config.sample_rate.0;
        match best_config {
            None => best_config = Some((stream_config, sample_format, rate)),
            Some((_, _, best_rate)) if rate > best_rate => {
                best_config = Some((stream_config, sample_format, rate))
            }
            _ => {}
        }
    }

    if let Some((config, format, _)) = best_config {
        Ok((config, format))
    } else {
        Err("未找到可用的音频输入配置，请检查麦克风连接".into())
    }
}

fn push_samples<T>(
    data: &[T],
    channels: usize,
    target: &Arc<Mutex<Vec<f32>>>,
    meter: Option<&Arc<Mutex<MeterEmitter>>>,
    dsp: Option<&Arc<Mutex<DspProcessor>>>,
) where
    T: Copy + Into<f32>,
{
    if channels == 0 {
        return;
    }
    let mut mono = Vec::with_capacity(data.len() / channels);
    for frame in data.chunks(channels) {
        let mut sum = 0.0;
        for sample in frame {
            sum += (*sample).into();
        }
        mono.push(sum / channels as f32);
    }

    let processed = if let Some(processor) = dsp {
        if let Ok(mut processor) = processor.lock() {
            processor.process_block(&mono)
        } else {
            mono.clone()
        }
    } else {
        mono.clone()
    };

    if let Some(meter) = meter {
        if let Ok(mut emitter) = meter.lock() {
            for value in &processed {
                emitter.push(value.abs());
            }
        }
    }

    if let Ok(mut buffer) = target.lock() {
        buffer.extend(processed.iter().copied());
    }
}

struct MeterContext {
    app: AppHandle,
    context: String,
    device_id: String,
}

impl MeterContext {
    fn new(app: AppHandle, context: String, device_id: String) -> Self {
        Self {
            app,
            context,
            device_id,
        }
    }

    fn emit(&self, peak: f32, rms: f32, vad_active: bool) {
        let payload = AudioMeterFrame {
            context: self.context.clone(),
            device_id: self.device_id.clone(),
            peak,
            rms,
            vad_active,
            timestamp_ms: current_timestamp_ms(),
        };
        let _ = self.app.emit("audio://meter", &payload);
    }
}

struct MeterEmitter {
    context: MeterContext,
    collector: MeterCollector,
}

impl MeterEmitter {
    fn new(context: MeterContext, sample_rate: u32) -> Self {
        Self {
            context,
            collector: MeterCollector::new(sample_rate),
        }
    }

    fn push(&mut self, amplitude: f32) {
        if let Some((peak, rms, vad_active)) = self.collector.push(amplitude) {
            self.context.emit(peak, rms, vad_active);
        }
    }

    fn flush(&mut self) {
        if let Some((peak, rms, vad_active)) = self.collector.flush() {
            self.context.emit(peak, rms, vad_active);
        }
    }
}

struct MeterCollector {
    window: usize,
    buffer: Vec<f32>,
    vad_active: bool,
}

impl MeterCollector {
    fn new(sample_rate: u32) -> Self {
        let window = compute_meter_window(sample_rate);
        Self::with_window(window)
    }

    fn with_window(window: usize) -> Self {
        let window = window.max(1);
        Self {
            window,
            buffer: Vec::with_capacity(window),
            vad_active: false,
        }
    }

    fn push(&mut self, amplitude: f32) -> Option<(f32, f32, bool)> {
        self.buffer.push(amplitude);
        if self.buffer.len() >= self.window {
            Some(self.consume())
        } else {
            None
        }
    }

    fn flush(&mut self) -> Option<(f32, f32, bool)> {
        if self.buffer.is_empty() {
            None
        } else {
            Some(self.consume())
        }
    }

    fn consume(&mut self) -> (f32, f32, bool) {
        let peak = self
            .buffer
            .iter()
            .copied()
            .fold(0.0_f32, |acc, value| acc.max(value));
        let rms = if self.buffer.is_empty() {
            0.0
        } else {
            let sum_sq: f32 = self.buffer.iter().map(|value| value * value).sum();
            (sum_sq / self.buffer.len() as f32).sqrt()
        };
        let next_vad_state = if self.vad_active {
            rms >= VAD_DEACTIVATE_THRESHOLD
        } else {
            rms >= VAD_ACTIVATE_THRESHOLD
        };
        self.vad_active = next_vad_state;
        self.buffer.clear();
        (peak, rms, self.vad_active)
    }
}

fn compute_meter_window(sample_rate: u32) -> usize {
    if sample_rate == 0 {
        return 1;
    }
    let window = (sample_rate as f32 / TARGET_METER_FPS).round() as usize;
    window.max(1)
}

fn current_timestamp_ms() -> u128 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meter_collector_emits_on_window() {
        let mut collector = MeterCollector::with_window(4);
        assert!(collector.push(0.1).is_none());
        assert!(collector.push(0.2).is_none());
        assert!(collector.push(0.3).is_none());
        let frame = collector
            .push(0.4)
            .expect("collector should emit once window is filled");
        assert!((frame.0 - 0.4).abs() < f32::EPSILON);
        let expected_rms =
            ((0.1_f32.powi(2) + 0.2_f32.powi(2) + 0.3_f32.powi(2) + 0.4_f32.powi(2)) / 4.0).sqrt();
        assert!((frame.1 - expected_rms).abs() < 1e-6);
        assert!(frame.2, "rms above threshold should mark VAD active");
    }

    #[test]
    fn meter_collector_flushes_remaining_samples() {
        let mut collector = MeterCollector::with_window(5);
        collector.push(0.25);
        collector.push(0.5);
        let frame = collector
            .flush()
            .expect("flush should emit even if window not filled");
        assert!((frame.0 - 0.5).abs() < f32::EPSILON);
        let expected_rms = ((0.25_f32.powi(2) + 0.5_f32.powi(2)) / 2.0).sqrt();
        assert!((frame.1 - expected_rms).abs() < 1e-6);
        assert!(
            frame.2,
            "collector flush above threshold should keep VAD active"
        );
        assert!(collector.flush().is_none(), "flush should clear buffer");
    }

    #[test]
    fn meter_collector_applies_vad_hysteresis() {
        let mut collector = MeterCollector::with_window(4);
        assert!(collector.push(0.05).is_none());
        assert!(collector.push(0.06).is_none());
        assert!(collector.push(0.07).is_none());
        let frame = collector
            .push(0.08)
            .expect("collector should emit when window fills");
        assert!(frame.2, "loud window should activate VAD");

        collector.push(0.005);
        collector.push(0.004);
        collector.push(0.003);
        let frame = collector
            .flush()
            .expect("flush should emit even if VAD drops");
        assert!(
            !frame.2,
            "window below release threshold should deactivate VAD"
        );
    }

    #[test]
    fn meter_collector_limits_frame_rate() {
        fn frame_count(sample_rate: u32) -> usize {
            let mut collector = MeterCollector::new(sample_rate);
            let mut frames = 0;
            for _ in 0..sample_rate {
                if collector.push(0.05).is_some() {
                    frames += 1;
                }
            }
            frames
        }

        for rate in [48_000_u32, 44_100_u32, 16_000_u32] {
            let frames = frame_count(rate);
            assert!(
                (30..=60).contains(&frames),
                "expected frame count within 30-60fps window for {rate}Hz, got {frames}"
            );
        }
    }

    #[test]
    fn sample_rate_converter_downsamples_48k_to_target() {
        let mut converter = SampleRateConverter::new(48_000, TARGET_SAMPLE_RATE);
        let input: Vec<f32> = (0..480).map(|idx| (idx as f32 / 480.0).sin()).collect();
        let output = converter.convert_block(&input);
        assert!(
            (output.len() as i32 - 160).abs() <= 1,
            "expected roughly 160 samples, got {}",
            output.len()
        );
        assert!(converter.flush().is_empty());
    }

    #[test]
    fn sample_rate_converter_streams_44100_hz() {
        let mut converter = SampleRateConverter::new(44_100, TARGET_SAMPLE_RATE);
        let chunk_a: Vec<f32> = (0..2205).map(|idx| (idx as f32 / 100.0).sin()).collect();
        let chunk_b: Vec<f32> = (0..2205)
            .map(|idx| ((idx + 2205) as f32 / 100.0).sin())
            .collect();
        let out_a = converter.convert_block(&chunk_a);
        let out_b = converter.convert_block(&chunk_b);
        let mut combined = Vec::new();
        combined.extend(out_a);
        combined.extend(out_b);
        combined.extend(converter.flush());
        assert!(
            (combined.len() as i32 - 1600).abs() <= 2,
            "expected approx 1600 samples after resampling, got {}",
            combined.len()
        );
        let max = combined
            .iter()
            .fold(0.0_f32, |acc, value| acc.max(value.abs()));
        assert!(max <= 1.0 + f32::EPSILON);
    }

    #[test]
    fn assess_noise_flags_excessive_noise() {
        let analytics = SampleAnalytics {
            peak_db: -6.0,
            rms_db: -12.0,
            noise_floor_db: -30.0,
            snr_db: 4.0,
        };
        let (alert, hint) = assess_noise(&analytics);
        assert!(
            alert,
            "noise alert should be raised when thresholds exceeded"
        );
        let detail = hint.expect("hint should be provided");
        assert!(
            detail.contains("-40"),
            "detail should reference noise floor limit"
        );
        assert!(
            detail.contains("10"),
            "detail should reference SNR threshold"
        );
    }

    #[test]
    fn assess_noise_accepts_quiet_environment() {
        let analytics = SampleAnalytics {
            peak_db: -8.0,
            rms_db: -16.0,
            noise_floor_db: -55.0,
            snr_db: 18.0,
        };
        let (alert, hint) = assess_noise(&analytics);
        assert!(!alert, "quiet environments should not trigger alerts");
        assert!(hint.is_none(), "no hint necessary when thresholds are met");
    }

    #[test]
    fn automatic_gain_control_boosts_quiet_audio() {
        let mut agc = AutomaticGainControl::new();
        let frame = vec![0.01_f32; 160];
        let processed = agc.process_frame(&frame);
        assert_eq!(processed.len(), frame.len());

        let rms = |samples: &[f32]| {
            (samples.iter().map(|value| value * value).sum::<f32>() / samples.len() as f32).sqrt()
        };

        let input_rms = rms(&frame);
        let output_rms = rms(&processed);
        assert!(
            output_rms > input_rms,
            "AGC should raise the RMS of quiet frames"
        );
        assert!(
            processed.iter().all(|value| value.abs() <= 1.0),
            "AGC output should remain within the normalised range"
        );
    }

    #[test]
    fn linear_upsample_interpolates_between_samples() {
        let frame = vec![0.0_f32, 1.0_f32];
        let upsampled = upsample_linear(&frame, 3);
        assert_eq!(upsampled.len(), 6);
        assert!((upsampled[0] - 0.0).abs() < 1e-6);
        assert!((upsampled[1] - (1.0 / 3.0)).abs() < 1e-6);
        assert!((upsampled[2] - (2.0 / 3.0)).abs() < 1e-6);
        assert!((upsampled[3] - 1.0).abs() < 1e-6);
        assert!((upsampled[5] - 1.0).abs() < 1e-6);
    }

    #[test]
    fn average_downsample_restores_original_resolution() {
        let mut high_res = vec![0.0_f32; 6];
        let template = [5.0_f32, 4.0, 3.0, 2.0, 1.0, 0.0];
        for (idx, value) in template.iter().enumerate() {
            high_res[idx] = value * 32_768.0;
        }
        let downsampled = downsample_average(&high_res, 3, 2);
        assert_eq!(downsampled.len(), 2);
        assert!(
            downsampled[0] > downsampled[1],
            "averages should follow the original trend"
        );
    }

    #[test]
    fn noise_suppressor_preserves_frame_length() {
        let mut suppressor = NoiseSuppressor::new(160);
        let frame: Vec<f32> = (0..160)
            .map(|idx| ((idx as f32) / 159.0 * 0.5) - 0.25)
            .collect();
        let processed = suppressor.process(&frame);
        assert_eq!(processed.len(), frame.len());
    }

    #[test]
    fn dsp_processor_emits_default_window_chunks() {
        let mut processor = DspProcessor::new(TARGET_SAMPLE_RATE, FrameWindowSetting::Default);
        let block = vec![0.05_f32; FrameWindowSetting::Default.samples()];
        let output = processor.process_block(&block);
        assert_eq!(output.len(), FrameWindowSetting::Default.samples());

        let partial = vec![0.05_f32; FrameWindowSetting::Fallback.samples()];
        let tail_output = processor.process_block(&partial);
        assert!(
            tail_output.is_empty(),
            "partial window should remain buffered"
        );

        let flushed = processor.flush();
        assert_eq!(
            flushed.len(),
            FrameWindowSetting::Fallback.samples(),
            "flush should release buffered remainder"
        );
        assert!(processor.flush().is_empty());
    }

    #[test]
    fn dsp_processor_supports_fallback_window() {
        let mut processor = DspProcessor::new(TARGET_SAMPLE_RATE, FrameWindowSetting::Fallback);
        let block = vec![0.05_f32; FrameWindowSetting::Fallback.samples()];
        let output = processor.process_block(&block);
        assert_eq!(output.len(), FrameWindowSetting::Fallback.samples());
        assert!(
            processor.flush().is_empty(),
            "no remainder expected after full window"
        );
    }
}

struct SampleAnalytics {
    peak_db: f32,
    rms_db: f32,
    noise_floor_db: f32,
    snr_db: f32,
}

fn analyze_samples(samples: &[f32]) -> SampleAnalytics {
    if samples.is_empty() {
        return SampleAnalytics {
            peak_db: f32::NEG_INFINITY,
            rms_db: f32::NEG_INFINITY,
            noise_floor_db: f32::NEG_INFINITY,
            snr_db: 0.0,
        };
    }

    let mut magnitudes: Vec<f32> = samples.iter().map(|s| s.abs()).collect();
    let peak = magnitudes
        .iter()
        .copied()
        .fold(0.0_f32, |acc, value| if value > acc { value } else { acc })
        .max(1e-6);
    let rms = (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt();
    magnitudes.sort_by(|a, b| match a.partial_cmp(b) {
        Some(Ordering::Less) => Ordering::Less,
        Some(Ordering::Greater) => Ordering::Greater,
        _ => Ordering::Equal,
    });
    let noise = if magnitudes.is_empty() {
        1e-6
    } else {
        let idx = ((magnitudes.len() as f32) * 0.1) as usize;
        magnitudes[idx.min(magnitudes.len() - 1)].max(1e-6)
    };

    let peak_db = 20.0 * peak.log10();
    let rms_db = 20.0 * rms.max(1e-6).log10();
    let noise_floor_db = 20.0 * noise.log10();
    let snr_db = 20.0 * (peak / noise).log10();

    SampleAnalytics {
        peak_db,
        rms_db,
        noise_floor_db,
        snr_db,
    }
}

fn assess_noise(analytics: &SampleAnalytics) -> (bool, Option<String>) {
    let mut warnings = Vec::new();
    if analytics.noise_floor_db > NOISE_FLOOR_LIMIT_DB {
        warnings.push(format!(
            "环境噪声 {:.1} dBFS 高于推荐上限 {:.1} dBFS",
            analytics.noise_floor_db, NOISE_FLOOR_LIMIT_DB
        ));
    }
    if analytics.snr_db < MIN_SNR_DB {
        warnings.push(format!(
            "信噪比 {:.1} dB 低于推荐值 {:.1} dB",
            analytics.snr_db, MIN_SNR_DB
        ));
    }

    if warnings.is_empty() {
        (false, None)
    } else {
        let detail = format!(
            "检测到噪音问题：{}。请尝试切换到更安静的环境或启用强降噪模式。",
            warnings.join("；")
        );
        (true, Some(detail))
    }
}

fn summarise_waveform(samples: &[f32], buckets: usize) -> Vec<f32> {
    if samples.is_empty() || buckets == 0 {
        return Vec::new();
    }

    let chunk = (samples.len() / buckets).max(1);
    let mut waveform = Vec::with_capacity(buckets);
    for idx in 0..buckets {
        let start = idx * chunk;
        if start >= samples.len() {
            break;
        }
        let end = ((idx + 1) * chunk).min(samples.len());
        let mut peak = 0.0;
        for sample in &samples[start..end] {
            let value = sample.abs();
            if value > peak {
                peak = value;
            }
        }
        waveform.push(peak);
    }
    waveform
}

fn encode_wave(samples: &[f32], sample_rate: u32) -> Result<Vec<u8>, String> {
    let spec = WavSpec {
        channels: 1,
        sample_rate,
        bits_per_sample: 16,
        sample_format: WavSampleFormat::Int,
    };
    let mut cursor = Cursor::new(Vec::new());
    {
        let mut writer =
            WavWriter::new(&mut cursor, spec).map_err(|err| format!("写入波形数据失败: {err}"))?;
        for sample in samples {
            let value = (sample * i16::MAX as f32).clamp(i16::MIN as f32, i16::MAX as f32) as i16;
            writer
                .write_sample(value)
                .map_err(|err| format!("写入音频样本失败: {err}"))?;
        }
        writer
            .finalize()
            .map_err(|err| format!("关闭音频缓冲失败: {err}"))?;
    }
    Ok(cursor.into_inner())
}

fn classify_device(label: &str) -> String {
    let lower = label.to_lowercase();
    if lower.contains("usb") {
        "usb".into()
    } else if lower.contains("bluetooth") || lower.contains("bt") {
        "bluetooth".into()
    } else if lower.contains("array") {
        "array".into()
    } else {
        "built-in".into()
    }
}

fn sanitize_identifier(label: &str) -> String {
    label
        .chars()
        .map(|ch| match ch {
            'a'..='z' | 'A'..='Z' | '0'..='9' => ch,
            _ => '-',
        })
        .collect()
}
