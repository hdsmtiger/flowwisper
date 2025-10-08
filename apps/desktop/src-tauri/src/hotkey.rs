use crate::audio::FrameWindowSetting;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use rand::{rngs::OsRng, RngCore};
use ring::{aead, hkdf, hmac};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    convert::TryInto,
    fs,
    fs::OpenOptions,
    io::{ErrorKind, Write},
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Duration, SystemTime},
};

#[cfg(any(target_os = "macos", target_os = "windows"))]
use std::collections::HashSet;

#[cfg(any(target_os = "macos", target_os = "windows"))]
use crate::native_probe;

#[cfg(any(target_os = "macos", target_os = "windows"))]
use rdev::{listen, EventType, Key};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Copy)]
#[serde(rename_all = "lowercase")]
pub enum HotkeySource {
    Fn,
    Custom,
}

impl Default for HotkeySource {
    fn default() -> Self {
        HotkeySource::Fn
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HotkeyBinding {
    pub combination: String,
    pub source: HotkeySource,
    pub reason: Option<String>,
}

impl Default for HotkeyBinding {
    fn default() -> Self {
        Self {
            combination: "Fn".into(),
            source: HotkeySource::Fn,
            reason: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FnProbeResult {
    pub supported: bool,
    pub latency_ms: Option<u128>,
    pub raw_latency_ns: Option<u128>,
    pub user_reaction_ms: Option<u128>,
    pub within_sla: Option<bool>,
    pub interface: Option<String>,
    pub device_origin: Option<String>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HotkeyConfigPayload {
    pub combination: String,
    pub source: HotkeySource,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HotkeyConfigEnvelope {
    pub payload: HotkeyConfigPayload,
    pub signature: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SampleEnvelope {
    pub version: u8,
    pub nonce: String,
    pub ciphertext: String,
    pub signature: String,
}

#[derive(Debug, Clone)]
pub struct SealedSample {
    pub token: String,
    pub path: PathBuf,
}

const SAMPLE_ENVELOPE_VERSION: u8 = 1;
const SAMPLE_ENVELOPE_AAD: &[u8] = b"device-sample";
const AUDIO_KEY_SALT: &[u8] = b"flowwisper.audio.cache.salt.v1";
const AUDIO_ENCRYPTION_INFO: &[u8] = b"flowwisper.audio.cache.enc.v1";
const AUDIO_HMAC_INFO: &[u8] = b"flowwisper.audio.cache.hmac.v1";
const SAMPLE_RETENTION_SECS: u64 = 60 * 60 * 24; // 24 小时窗口
const SAMPLE_RETENTION_CAPACITY: usize = 5; // 最近 5 份诊断样本

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FnProbeLogEntry {
    pub timestamp_ms: u128,
    pub supported: bool,
    pub latency_ms: Option<u128>,
    pub raw_latency_ns: Option<u128>,
    pub user_reaction_ms: Option<u128>,
    pub within_sla: Option<bool>,
    pub interface: Option<String>,
    pub device_origin: Option<String>,
    pub reason: Option<String>,
}

pub struct AppState {
    pub session: crate::session::SessionStateManager,
    pub hotkey: Mutex<HotkeyState>,
    config_path: PathBuf,
    pub probe_log_path: PathBuf,
    hmac_key: Vec<u8>,
    audio_keys: AudioCacheKeys,
    onboarding_config_path: PathBuf,
    pub onboarding: Mutex<OnboardingPreferences>,
    sample_dir: PathBuf,
    frame_window: Mutex<FrameWindowState>,
}

#[derive(Debug, Default, PartialEq, Eq)]
struct SampleCleanupStats {
    removed: usize,
    retained: usize,
    errors: usize,
}

#[derive(Debug, Default)]
struct FrameWindowState {
    mode: FrameWindowSetting,
    last_reason: Option<String>,
}

#[derive(Debug, Default)]
pub struct HotkeyState {
    pub binding: HotkeyBinding,
    pub last_probe: Option<FnProbeResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum CalibrationMode {
    Auto,
    Manual,
}

impl Default for CalibrationMode {
    fn default() -> Self {
        CalibrationMode::Auto
    }
}

impl std::fmt::Display for CalibrationMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CalibrationMode::Auto => write!(f, "auto"),
            CalibrationMode::Manual => write!(f, "manual"),
        }
    }
}

impl std::str::FromStr for CalibrationMode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "auto" => Ok(CalibrationMode::Auto),
            "manual" => Ok(CalibrationMode::Manual),
            other => Err(format!("未知的校准模式: {other}")),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SavedCalibration {
    pub threshold: f32,
    pub noise_floor_db: f32,
    pub sample_window_ms: u32,
    pub device_label: Option<String>,
    pub mode: CalibrationMode,
    pub recommended_threshold: Option<f32>,
    pub updated_at_ms: Option<u128>,
    #[serde(default)]
    pub noise_alert: bool,
    #[serde(default)]
    pub noise_hint: Option<String>,
    #[serde(default)]
    pub strong_noise_mode: bool,
    #[serde(default)]
    pub frame_window_ms: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PermissionTracker {
    #[serde(default)]
    pub microphone: bool,
    #[serde(default)]
    pub accessibility: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TutorialStatus {
    Completed,
    Skipped,
}

impl std::fmt::Display for TutorialStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TutorialStatus::Completed => write!(f, "completed"),
            TutorialStatus::Skipped => write!(f, "skipped"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct OnboardingPreferences {
    pub engine_choice: Option<String>,
    pub calibrations: HashMap<String, SavedCalibration>,
    #[serde(default)]
    pub tutorial_completed: bool,
    #[serde(default)]
    pub tutorial_status: Option<TutorialStatus>,
    #[serde(default)]
    pub permissions: PermissionTracker,
    #[serde(default)]
    pub selected_microphone: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OnboardingConfigEnvelope {
    pub payload: OnboardingPreferences,
    pub signature: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct AudioCacheKeys {
    encryption: [u8; 32],
    hmac: [u8; 32],
}

impl AudioCacheKeys {
    fn encryption_key(&self) -> &[u8; 32] {
        &self.encryption
    }

    fn signing_key(&self) -> &[u8; 32] {
        &self.hmac
    }
}

impl AppState {
    pub fn new(config_path: PathBuf, hmac_key: Vec<u8>, binding: HotkeyBinding) -> Self {
        let probe_log_path = config_path
            .parent()
            .map(|dir| dir.join("fn_probe_log.json"))
            .unwrap_or_else(|| PathBuf::from("fn_probe_log.json"));
        let onboarding_config_path = config_path
            .parent()
            .map(|dir| dir.join("onboarding.json"))
            .unwrap_or_else(|| PathBuf::from("onboarding.json"));
        let onboarding = load_onboarding_preferences(&onboarding_config_path, &hmac_key);
        let sample_dir = config_path
            .parent()
            .map(|dir| dir.join("samples"))
            .unwrap_or_else(|| PathBuf::from("samples"));
        let audio_keys =
            derive_audio_cache_keys(&hmac_key).expect("failed to derive audio cache keys");
        Self {
            session: crate::session::SessionStateManager::new(),
            hotkey: Mutex::new(HotkeyState {
                binding,
                last_probe: None,
            }),
            config_path,
            probe_log_path,
            hmac_key,
            audio_keys,
            onboarding_config_path,
            onboarding: Mutex::new(onboarding),
            sample_dir,
            frame_window: Mutex::new(FrameWindowState::default()),
        }
    }

    pub fn persist_binding(&self, binding: &HotkeyBinding) -> Result<(), String> {
        let payload = HotkeyConfigPayload {
            combination: binding.combination.clone(),
            source: binding.source,
            reason: binding.reason.clone(),
        };
        let signature = sign_payload(&self.hmac_key, &payload)?;
        let envelope = HotkeyConfigEnvelope { payload, signature };
        if let Some(parent) = self.config_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|err| format!("failed to prepare config directory: {err}"))?;
        }
        let bytes = serde_json::to_vec_pretty(&envelope)
            .map_err(|err| format!("failed to serialize hotkey config: {err}"))?;
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&self.config_path)
            .map_err(|err| format!("failed to open config file: {err}"))?;
        #[cfg(unix)]
        {
            let perm = fs::Permissions::from_mode(0o600);
            file.set_permissions(perm)
                .map_err(|err| format!("failed to set config permissions: {err}"))?;
        }
        file.write_all(&bytes)
            .map_err(|err| format!("failed to persist hotkey config: {err}"))?;
        Ok(())
    }

    pub fn frame_window_mode(&self) -> FrameWindowSetting {
        self.frame_window
            .lock()
            .map(|state| state.mode)
            .unwrap_or_default()
    }

    pub fn set_frame_window(
        &self,
        mode: FrameWindowSetting,
        reason: Option<String>,
    ) -> FrameWindowSetting {
        if let Ok(mut guard) = self.frame_window.lock() {
            guard.mode = mode;
            guard.last_reason = reason;
        }
        mode
    }

    pub fn frame_window_reason(&self) -> Option<String> {
        self.frame_window
            .lock()
            .ok()
            .and_then(|state| state.last_reason.clone())
    }

    pub fn append_probe_log(&self, probe: &FnProbeResult) -> Result<(), String> {
        let mut entries: Vec<FnProbeLogEntry> = fs::read(&self.probe_log_path)
            .ok()
            .and_then(|raw| serde_json::from_slice(&raw).ok())
            .unwrap_or_default();
        let timestamp_ms = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let entry = FnProbeLogEntry {
            timestamp_ms,
            supported: probe.supported,
            latency_ms: probe.latency_ms,
            raw_latency_ns: probe.raw_latency_ns,
            user_reaction_ms: probe.user_reaction_ms,
            within_sla: probe.within_sla,
            interface: probe.interface.clone(),
            device_origin: probe.device_origin.clone(),
            reason: probe.reason.clone(),
        };
        entries.push(entry);
        if entries.len() > 20 {
            let start = entries.len() - 20;
            entries = entries[start..].to_vec();
        }
        if let Some(parent) = self.probe_log_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|err| format!("failed to prepare probe log directory: {err}"))?;
        }
        let encoded = serde_json::to_vec_pretty(&entries)
            .map_err(|err| format!("failed to encode probe log: {err}"))?;
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&self.probe_log_path)
            .map_err(|err| format!("failed to open probe log: {err}"))?;
        #[cfg(unix)]
        {
            let perm = fs::Permissions::from_mode(0o600);
            file.set_permissions(perm)
                .map_err(|err| format!("failed to set probe log permissions: {err}"))?;
        }
        file.write_all(&encoded)
            .map_err(|err| format!("failed to persist probe log: {err}"))?;
        Ok(())
    }

    fn persist_onboarding_preferences(&self, prefs: &OnboardingPreferences) -> Result<(), String> {
        if let Some(parent) = self.onboarding_config_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|err| format!("failed to prepare onboarding directory: {err}"))?;
        }
        let signature = sign_onboarding_preferences(&self.hmac_key, prefs)
            .map_err(|err| format!("failed to sign onboarding preferences: {err}"))?;
        let envelope = OnboardingConfigEnvelope {
            payload: prefs.clone(),
            signature,
        };
        let bytes = serde_json::to_vec_pretty(&envelope)
            .map_err(|err| format!("failed to encode onboarding preferences: {err}"))?;
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&self.onboarding_config_path)
            .map_err(|err| format!("failed to open onboarding preferences: {err}"))?;
        #[cfg(unix)]
        {
            let perm = fs::Permissions::from_mode(0o600);
            file.set_permissions(perm)
                .map_err(|err| format!("failed to set onboarding permissions: {err}"))?;
        }
        file.write_all(&bytes)
            .map_err(|err| format!("failed to persist onboarding preferences: {err}"))?;
        Ok(())
    }

    pub fn update_engine_choice(&self, choice: &str) -> Result<(), String> {
        let mut guard = self
            .onboarding
            .lock()
            .map_err(|err| format!("failed to update engine choice: {err}"))?;
        guard.engine_choice = Some(choice.to_string());
        self.persist_onboarding_preferences(&guard)
    }

    pub fn engine_choice(&self) -> Option<String> {
        self.onboarding
            .lock()
            .ok()
            .and_then(|prefs| prefs.engine_choice.clone())
    }

    pub fn update_permission_status(&self, key: &str, granted: bool) -> Result<(), String> {
        let mut guard = self
            .onboarding
            .lock()
            .map_err(|err| format!("failed to persist permission state: {err}"))?;
        match key {
            "microphone" => guard.permissions.microphone = granted,
            "accessibility" => guard.permissions.accessibility = granted,
            other => {
                return Err(format!("unknown permission key: {other}"));
            }
        }
        self.persist_onboarding_preferences(&guard)
    }

    pub fn permission_status(&self) -> PermissionTracker {
        self.onboarding
            .lock()
            .map(|prefs| prefs.permissions.clone())
            .unwrap_or_default()
    }

    pub fn save_calibration(
        &self,
        device_id: &str,
        calibration: SavedCalibration,
    ) -> Result<(), String> {
        let mut guard = self
            .onboarding
            .lock()
            .map_err(|err| format!("failed to persist calibration: {err}"))?;
        let mut updated = calibration;
        if updated.recommended_threshold.is_none() {
            if let Some(existing) = guard.calibrations.get(device_id) {
                updated.recommended_threshold =
                    existing.recommended_threshold.or(Some(existing.threshold));
            }
        }
        if updated.device_label.is_none() {
            if let Some(existing) = guard.calibrations.get(device_id) {
                updated.device_label = existing.device_label.clone();
            }
        }
        if updated.mode == CalibrationMode::Manual && updated.recommended_threshold.is_none() {
            updated.recommended_threshold = Some(updated.threshold);
        }
        if updated.frame_window_ms.is_none() {
            updated.frame_window_ms = Some(self.frame_window_mode().duration_ms());
        }
        updated.updated_at_ms = Some(current_timestamp_ms());
        guard.calibrations.insert(device_id.to_string(), updated);
        self.persist_onboarding_preferences(&guard)
    }

    pub fn calibration_for(&self, device_id: &str) -> Option<SavedCalibration> {
        self.onboarding
            .lock()
            .ok()
            .and_then(|prefs| prefs.calibrations.get(device_id).cloned())
    }

    fn mark_tutorial_outcome(&self, outcome: TutorialStatus) -> Result<(), String> {
        let mut guard = self
            .onboarding
            .lock()
            .map_err(|err| format!("failed to persist tutorial outcome: {err}"))?;
        guard.tutorial_status = Some(outcome.clone());
        guard.tutorial_completed = matches!(outcome, TutorialStatus::Completed);
        self.persist_onboarding_preferences(&guard)
    }

    pub fn mark_tutorial_complete(&self) -> Result<(), String> {
        self.mark_tutorial_outcome(TutorialStatus::Completed)
    }

    pub fn mark_tutorial_skipped(&self) -> Result<(), String> {
        self.mark_tutorial_outcome(TutorialStatus::Skipped)
    }

    pub fn tutorial_completed(&self) -> bool {
        self.onboarding
            .lock()
            .map(|prefs| {
                if let Some(status) = &prefs.tutorial_status {
                    matches!(status, TutorialStatus::Completed | TutorialStatus::Skipped)
                } else {
                    prefs.tutorial_completed
                }
            })
            .unwrap_or(false)
    }

    pub fn tutorial_status(&self) -> Option<TutorialStatus> {
        self.onboarding
            .lock()
            .ok()
            .and_then(|prefs| prefs.tutorial_status.clone())
    }

    pub fn selected_microphone(&self) -> Option<String> {
        self.onboarding
            .lock()
            .ok()
            .and_then(|prefs| prefs.selected_microphone.clone())
    }

    pub fn persist_selected_microphone(&self, device_id: Option<String>) -> Result<(), String> {
        let mut guard = self
            .onboarding
            .lock()
            .map_err(|err| format!("failed to persist microphone selection: {err}"))?;
        guard.selected_microphone = device_id;
        self.persist_onboarding_preferences(&guard)
    }

    pub fn onboarding_config_path(&self) -> &PathBuf {
        &self.onboarding_config_path
    }

    pub fn sample_dir(&self) -> &PathBuf {
        &self.sample_dir
    }

    pub fn hmac_key(&self) -> &[u8] {
        &self.hmac_key
    }

    pub fn store_device_sample(
        &self,
        token: &str,
        wav_bytes: &[u8],
    ) -> Result<SealedSample, String> {
        if token.is_empty() {
            return Err("sample token cannot be empty".into());
        }
        if let Err(err) = self.cleanup_samples() {
            eprintln!("sample cleanup (pre-store) failed: {}", err);
        }
        fs::create_dir_all(&self.sample_dir)
            .map_err(|err| format!("failed to prepare sample directory: {err}"))?;
        let envelope = self
            .seal_sample_payload(wav_bytes)
            .map_err(|err| format!("failed to encrypt device sample: {err}"))?;
        let encoded = serde_json::to_vec_pretty(&envelope)
            .map_err(|err| format!("failed to encode sample envelope: {err}"))?;
        let path = self.sample_envelope_path(token);
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&path)
            .map_err(|err| format!("failed to open sample envelope: {err}"))?;
        #[cfg(unix)]
        {
            let perm = fs::Permissions::from_mode(0o600);
            file.set_permissions(perm)
                .map_err(|err| format!("failed to secure sample envelope: {err}"))?;
        }
        file.write_all(&encoded)
            .map_err(|err| format!("failed to persist sample envelope: {err}"))?;
        if let Err(err) = self.cleanup_samples() {
            eprintln!("sample cleanup (post-store) failed: {}", err);
        }
        Ok(SealedSample {
            token: token.to_string(),
            path,
        })
    }

    pub fn load_device_sample(&self, token: &str) -> Result<Vec<u8>, String> {
        if token.is_empty() {
            return Err("sample token cannot be empty".into());
        }
        match self.cleanup_samples() {
            Ok(stats) => {
                if stats.errors > 0 {
                    eprintln!(
                        "sample cleanup (pre-load) encountered {} errors",
                        stats.errors
                    );
                }
            }
            Err(err) => {
                eprintln!("sample cleanup (pre-load) failed: {}", err);
            }
        }
        let path = self.sample_envelope_path(token);
        let raw =
            fs::read(&path).map_err(|err| format!("failed to read sample envelope: {err}"))?;
        let envelope: SampleEnvelope = serde_json::from_slice(&raw)
            .map_err(|err| format!("failed to decode sample envelope: {err}"))?;
        self.open_sample_payload(envelope)
            .map_err(|err| format!("failed to decrypt device sample: {err}"))
    }

    fn sample_envelope_path(&self, token: &str) -> PathBuf {
        let mut filename = token.to_string();
        if !filename.ends_with(".json") {
            filename.push_str(".json");
        }
        self.sample_dir.join(filename)
    }

    fn seal_sample_payload(&self, wav_bytes: &[u8]) -> Result<SampleEnvelope, String> {
        let mut nonce = [0u8; 12];
        OsRng
            .try_fill_bytes(&mut nonce)
            .map_err(|err| format!("failed to generate sample nonce: {err}"))?;
        let key = aead::UnboundKey::new(&aead::AES_256_GCM, self.audio_keys.encryption_key())
            .map_err(|_| "invalid sample encryption key material".to_string())?;
        let key = aead::LessSafeKey::new(key);
        let mut buffer = wav_bytes.to_vec();
        buffer.reserve(aead::AES_256_GCM.tag_len());
        key.seal_in_place_append_tag(
            aead::Nonce::assume_unique_for_key(nonce),
            aead::Aad::from(SAMPLE_ENVELOPE_AAD),
            &mut buffer,
        )
        .map_err(|_| "failed to seal sample payload".to_string())?;
        let mut signed = Vec::with_capacity(nonce.len() + buffer.len());
        signed.extend_from_slice(&nonce);
        signed.extend_from_slice(&buffer);
        let signing_key = hmac::Key::new(hmac::HMAC_SHA256, self.audio_keys.signing_key());
        let signature = hmac::sign(&signing_key, &signed);
        Ok(SampleEnvelope {
            version: SAMPLE_ENVELOPE_VERSION,
            nonce: BASE64.encode(nonce),
            ciphertext: BASE64.encode(buffer),
            signature: BASE64.encode(signature.as_ref()),
        })
    }

    fn open_sample_payload(&self, envelope: SampleEnvelope) -> Result<Vec<u8>, String> {
        if envelope.version != SAMPLE_ENVELOPE_VERSION {
            return Err(format!(
                "unsupported sample envelope version: {}",
                envelope.version
            ));
        }
        let nonce_bytes = BASE64
            .decode(envelope.nonce.as_bytes())
            .map_err(|err| format!("failed to decode sample nonce: {err}"))?;
        if nonce_bytes.len() != 12 {
            return Err("invalid sample nonce length".into());
        }
        let ciphertext = BASE64
            .decode(envelope.ciphertext.as_bytes())
            .map_err(|err| format!("failed to decode sample ciphertext: {err}"))?;
        let signature = BASE64
            .decode(envelope.signature.as_bytes())
            .map_err(|err| format!("failed to decode sample signature: {err}"))?;
        let mut signed = Vec::with_capacity(nonce_bytes.len() + ciphertext.len());
        signed.extend_from_slice(&nonce_bytes);
        signed.extend_from_slice(&ciphertext);
        let signing_key = hmac::Key::new(hmac::HMAC_SHA256, self.audio_keys.signing_key());
        hmac::verify(&signing_key, &signed, &signature)
            .map_err(|_| "sample signature mismatch".to_string())?;
        let nonce: [u8; 12] = nonce_bytes
            .as_slice()
            .try_into()
            .map_err(|_| "invalid nonce length".to_string())?;
        let key = aead::UnboundKey::new(&aead::AES_256_GCM, self.audio_keys.encryption_key())
            .map_err(|_| "invalid sample encryption key material".to_string())?;
        let key = aead::LessSafeKey::new(key);
        let mut buffer = ciphertext;
        let decrypted = key
            .open_in_place(
                aead::Nonce::assume_unique_for_key(nonce),
                aead::Aad::from(SAMPLE_ENVELOPE_AAD),
                &mut buffer,
            )
            .map_err(|_| "failed to decrypt sample payload".to_string())?;
        Ok(decrypted.to_vec())
    }

    fn cleanup_samples(&self) -> Result<SampleCleanupStats, String> {
        let mut stats = SampleCleanupStats::default();
        let entries = match fs::read_dir(&self.sample_dir) {
            Ok(entries) => entries,
            Err(err) if err.kind() == ErrorKind::NotFound => return Ok(stats),
            Err(err) => {
                return Err(format!(
                    "failed to read sample directory {}: {err}",
                    self.sample_dir.display()
                ))
            }
        };

        let mut retained: Vec<(u128, PathBuf)> = Vec::new();
        let now = SystemTime::now();
        let retention = Duration::from_secs(SAMPLE_RETENTION_SECS);

        for entry in entries {
            let entry = entry.map_err(|err| format!("failed to iterate samples: {err}"))?;
            let path = entry.path();
            let file_type = entry
                .file_type()
                .map_err(|err| format!("failed to inspect sample entry: {err}"))?;
            if !file_type.is_file() {
                continue;
            }
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }

            let metadata = entry
                .metadata()
                .map_err(|err| format!("failed to read sample metadata: {err}"))?;
            let modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            let age = now.duration_since(modified).unwrap_or(Duration::ZERO);
            if age > retention {
                match fs::remove_file(&path) {
                    Ok(_) => stats.removed += 1,
                    Err(err) => {
                        stats.errors += 1;
                        eprintln!(
                            "sample cleanup: failed to remove expired sample {}: {err}",
                            path.display()
                        );
                    }
                }
                continue;
            }

            let timestamp = modified
                .duration_since(SystemTime::UNIX_EPOCH)
                .map(|duration| duration.as_millis())
                .unwrap_or(0);
            retained.push((timestamp, path));
        }

        if retained.len() > SAMPLE_RETENTION_CAPACITY {
            retained.sort_by_key(|(timestamp, _)| *timestamp);
            let overflow = retained.len() - SAMPLE_RETENTION_CAPACITY;
            for (_, path) in retained.iter().take(overflow) {
                match fs::remove_file(path) {
                    Ok(_) => stats.removed += 1,
                    Err(err) => {
                        stats.errors += 1;
                        eprintln!(
                            "sample cleanup: failed to evict sample {}: {err}",
                            path.display()
                        );
                    }
                }
            }
            retained.drain(..overflow);
        }

        stats.retained = retained.len();
        Ok(stats)
    }
}

fn derive_audio_cache_keys(master: &[u8]) -> Result<AudioCacheKeys, String> {
    if master.len() < 32 {
        return Err("master key material must be at least 32 bytes".into());
    }
    let salt = hkdf::Salt::new(hkdf::HKDF_SHA256, AUDIO_KEY_SALT);
    let prk = salt.extract(master);

    let mut encryption = [0u8; 32];
    let mut signing = [0u8; 32];

    prk.expand(&[AUDIO_ENCRYPTION_INFO], hkdf::HKDF_SHA256)
        .map_err(|_| "failed to derive audio encryption key".to_string())?
        .fill(&mut encryption)
        .map_err(|_| "failed to fill audio encryption key".to_string())?;

    prk.expand(&[AUDIO_HMAC_INFO], hkdf::HKDF_SHA256)
        .map_err(|_| "failed to derive audio signing key".to_string())?
        .fill(&mut signing)
        .map_err(|_| "failed to fill audio signing key".to_string())?;

    Ok(AudioCacheKeys {
        encryption,
        hmac: signing,
    })
}

fn current_timestamp_ms() -> u128 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}

fn load_onboarding_preferences(path: &PathBuf, key: &[u8]) -> OnboardingPreferences {
    fs::read(path)
        .ok()
        .and_then(|raw| serde_json::from_slice::<OnboardingConfigEnvelope>(&raw).ok())
        .and_then(|envelope| verify_onboarding_envelope(key, envelope).ok())
        .unwrap_or_default()
}

impl From<HotkeyConfigPayload> for HotkeyBinding {
    fn from(value: HotkeyConfigPayload) -> Self {
        Self {
            combination: value.combination,
            source: value.source,
            reason: value.reason,
        }
    }
}

pub fn sign_payload(key: &[u8], payload: &HotkeyConfigPayload) -> Result<String, String> {
    let serialized = serde_json::to_vec(payload)
        .map_err(|err| format!("failed to encode hotkey payload: {err}"))?;
    let signing_key = hmac::Key::new(hmac::HMAC_SHA256, key);
    let tag = hmac::sign(&signing_key, &serialized);
    Ok(BASE64.encode(tag.as_ref()))
}

fn sign_onboarding_preferences(
    key: &[u8],
    prefs: &OnboardingPreferences,
) -> Result<String, String> {
    let serialized = serde_json::to_vec(prefs)
        .map_err(|err| format!("failed to encode onboarding payload: {err}"))?;
    let signing_key = hmac::Key::new(hmac::HMAC_SHA256, key);
    let tag = hmac::sign(&signing_key, &serialized);
    Ok(BASE64.encode(tag.as_ref()))
}

pub fn verify_envelope(
    key: &[u8],
    envelope: HotkeyConfigEnvelope,
) -> Result<HotkeyBinding, String> {
    let serialized = serde_json::to_vec(&envelope.payload)
        .map_err(|err| format!("failed to encode hotkey payload: {err}"))?;
    let signing_key = hmac::Key::new(hmac::HMAC_SHA256, key);
    let signature = BASE64
        .decode(envelope.signature.as_bytes())
        .map_err(|err| format!("failed to decode hotkey signature: {err}"))?;
    hmac::verify(&signing_key, &serialized, &signature)
        .map_err(|_| "hotkey signature mismatch".to_string())?;
    Ok(HotkeyBinding::from(envelope.payload))
}

fn verify_onboarding_envelope(
    key: &[u8],
    envelope: OnboardingConfigEnvelope,
) -> Result<OnboardingPreferences, String> {
    let serialized = serde_json::to_vec(&envelope.payload)
        .map_err(|err| format!("failed to encode onboarding payload: {err}"))?;
    let signing_key = hmac::Key::new(hmac::HMAC_SHA256, key);
    let signature = BASE64
        .decode(envelope.signature.as_bytes())
        .map_err(|err| format!("failed to decode onboarding signature: {err}"))?;
    hmac::verify(&signing_key, &serialized, &signature)
        .map_err(|_| "onboarding signature mismatch".to_string())?;
    Ok(envelope.payload)
}

pub fn load_or_create_hmac_key(config_path: &Path) -> Result<Vec<u8>, String> {
    let dir = config_path
        .parent()
        .ok_or_else(|| "config path missing parent directory".to_string())?;
    fs::create_dir_all(dir).map_err(|err| format!("failed to prepare config directory: {err}"))?;

    #[cfg(target_os = "macos")]
    {
        use security_framework::passwords::{get_generic_password, set_generic_password};

        const SERVICE: &str = "flowwisper.hotkey";
        const ACCOUNT: &str = "fn_hmac";
        const ERR_SEC_ITEM_NOT_FOUND: i32 = -25300;

        match get_generic_password(SERVICE, ACCOUNT) {
            Ok(secret) => return Ok(secret),
            Err(err) if err.code() == ERR_SEC_ITEM_NOT_FOUND => {
                let mut generated = [0u8; 32];
                OsRng
                    .try_fill_bytes(&mut generated)
                    .map_err(|e| format!("failed to generate hotkey secret: {e}"))?;
                set_generic_password(SERVICE, ACCOUNT, &generated)
                    .map_err(|e| format!("failed to persist hotkey secret: {e}"))?;
                return Ok(generated.to_vec());
            }
            Err(err) => {
                return Err(format!("failed to access keychain item: {err}"));
            }
        }
    }

    #[cfg(target_os = "windows")]
    {
        use windows_dpapi::{protect_data, unprotect_data, ProtectionScope};

        let sealed_path = dir.join("hotkey.secret");
        if let Ok(blob) = fs::read(&sealed_path) {
            if let Ok(secret) = unprotect_data(&blob, None) {
                if secret.len() == 32 {
                    return Ok(secret);
                }
            }
        }

        let mut generated = [0u8; 32];
        OsRng
            .try_fill_bytes(&mut generated)
            .map_err(|e| format!("failed to generate hotkey secret: {e}"))?;
        let protected = protect_data(&generated, None, ProtectionScope::User)
            .map_err(|e| format!("failed to protect hotkey secret: {e}"))?;
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&sealed_path)
            .map_err(|e| format!("failed to open secret container: {e}"))?;
        file.write_all(&protected)
            .map_err(|e| format!("failed to persist protected secret: {e}"))?;
        return Ok(generated.to_vec());
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        let key_path = dir.join("hotkey.key");
        if let Ok(existing) = fs::read(&key_path) {
            if existing.len() == 32 {
                return Ok(existing);
            }
        }

        let mut secret = [0u8; 32];
        OsRng
            .try_fill_bytes(&mut secret)
            .map_err(|err| format!("failed to generate hotkey secret: {err}"))?;
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&key_path)
            .map_err(|err| format!("failed to open secret file: {err}"))?;
        #[cfg(unix)]
        {
            let perm = fs::Permissions::from_mode(0o600);
            file.set_permissions(perm)
                .map_err(|err| format!("failed to set secret permissions: {err}"))?;
        }
        file.write_all(&secret)
            .map_err(|err| format!("failed to persist hotkey secret: {err}"))?;
        return Ok(secret.to_vec());
    }

    #[allow(unreachable_code)]
    Err("platform not supported".into())
}

pub fn load_hotkey_config(path: &Path, key: &[u8]) -> Option<HotkeyBinding> {
    fs::read(path).ok().and_then(|raw| {
        serde_json::from_slice::<HotkeyConfigEnvelope>(&raw)
            .ok()
            .and_then(|envelope| verify_envelope(key, envelope).ok())
    })
}

pub struct HotkeyCompatibilityLayer;

impl HotkeyCompatibilityLayer {
    pub const RESERVED: &'static [&'static str] = &[
        "Ctrl+Alt+Delete",
        "Ctrl+Shift+Esc",
        "Alt+F4",
        "Alt+Tab",
        "Cmd+Q",
        "Cmd+Option+Esc",
        "Cmd+Space",
        "Win+L",
        "Win+Space",
    ];

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    const SLA: Duration = Duration::from_millis(400);

    pub fn probe_fn() -> FnProbeResult {
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        {
            if let Ok(value) = std::env::var("FLOWWISPER_SIMULATE_FN") {
                if value.eq_ignore_ascii_case("supported") {
                    return FnProbeResult {
                        supported: true,
                        latency_ms: Some(1),
                        raw_latency_ns: None,
                        user_reaction_ms: Some(0),
                        within_sla: Some(true),
                        interface: Some("simulated".into()),
                        device_origin: Some("Simulated".into()),
                        reason: None,
                    };
                } else if value.eq_ignore_ascii_case("unsupported") {
                    return FnProbeResult {
                        supported: false,
                        latency_ms: None,
                        raw_latency_ns: None,
                        user_reaction_ms: None,
                        within_sla: None,
                        interface: Some("simulated".into()),
                        device_origin: Some("Simulated".into()),
                        reason: Some("模拟环境标记为不支持 Fn 捕获".into()),
                    };
                }
            }

            match native_probe::probe_fn(Self::SLA) {
                Ok(observation) => FnProbeResult {
                    supported: observation.supported,
                    latency_ms: observation.latency.map(|d| d.as_millis().max(1)),
                    raw_latency_ns: observation.raw_latency_ns,
                    user_reaction_ms: observation.user_reaction.map(|d| d.as_millis()),
                    within_sla: observation.within_sla,
                    interface: Some(observation.interface.into()),
                    device_origin: observation.device_origin,
                    reason: observation.reason,
                },
                Err(native_probe::ProbeError::Timeout) => FnProbeResult {
                    supported: false,
                    latency_ms: None,
                    raw_latency_ns: None,
                    user_reaction_ms: None,
                    within_sla: None,
                    interface: None,
                    device_origin: None,
                    reason: Some(format!(
                        "未在 {}ms 内收到驱动回调，已建议录制备用组合",
                        Self::SLA.as_millis()
                    )),
                },
                Err(native_probe::ProbeError::Io(err)) => FnProbeResult {
                    supported: false,
                    latency_ms: None,
                    raw_latency_ns: None,
                    user_reaction_ms: None,
                    within_sla: None,
                    interface: None,
                    device_origin: None,
                    reason: Some(format!("原生探测失败: {err}")),
                },
            }
        }

        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            FnProbeResult {
                supported: false,
                latency_ms: None,
                raw_latency_ns: None,
                user_reaction_ms: None,
                within_sla: None,
                interface: None,
                device_origin: None,
                reason: Some("当前平台尚未实现 Fn 捕获，请改用备用组合。".into()),
            }
        }
    }

    pub fn capture_custom(timeout: Duration) -> Result<String, String> {
        #[cfg(any(target_os = "macos", target_os = "windows"))]
        {
            use std::sync::mpsc;
            use std::thread;

            let (tx, rx) = mpsc::channel();
            thread::spawn(move || {
                let mut modifiers: HashSet<Key> = HashSet::new();
                let sender = tx.clone();
                let _ = std::panic::catch_unwind(move || {
                    let _ = listen(move |event| match event.event_type {
                        EventType::KeyPress(key) => {
                            if Self::is_modifier_key(key) {
                                modifiers.insert(key);
                                return;
                            }
                            if key == Key::Function {
                                let _ = sender
                                    .send(CaptureSignal::Error("Fn 键无法作为备用组合主键".into()));
                                panic!("stop-listener");
                            }
                            if let Some(combo) = Self::build_combination(&modifiers, key) {
                                let _ = sender.send(CaptureSignal::Captured(combo));
                            } else {
                                let _ =
                                    sender.send(CaptureSignal::Error("未识别的按键组合".into()));
                            }
                            panic!("stop-listener");
                        }
                        EventType::KeyRelease(key) => {
                            if Self::is_modifier_key(key) {
                                modifiers.remove(&key);
                            }
                        }
                        _ => {}
                    });
                });
            });

            match rx.recv_timeout(timeout) {
                Ok(CaptureSignal::Captured(combo)) => Ok(combo),
                Ok(CaptureSignal::Error(reason)) => Err(reason),
                Err(_) => Err("等待组合键超时，请重试".into()),
            }
        }

        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            let _ = timeout;
            Err("当前平台未提供热键捕获能力".into())
        }
    }

    pub fn conflicts() -> Vec<String> {
        Self::RESERVED.iter().map(|item| item.to_string()).collect()
    }

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    fn is_modifier_key(key: Key) -> bool {
        matches!(
            key,
            Key::ShiftLeft
                | Key::ShiftRight
                | Key::ControlLeft
                | Key::ControlRight
                | Key::Alt
                | Key::AltGr
                | Key::MetaLeft
                | Key::MetaRight
        )
    }

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    fn describe_key(key: Key) -> Option<String> {
        match key {
            Key::Space => Some("Space".into()),
            Key::Escape => Some("Esc".into()),
            Key::Tab => Some("Tab".into()),
            Key::Return => Some("Enter".into()),
            Key::Backspace => Some("Backspace".into()),
            Key::UpArrow => Some("ArrowUp".into()),
            Key::DownArrow => Some("ArrowDown".into()),
            Key::LeftArrow => Some("ArrowLeft".into()),
            Key::RightArrow => Some("ArrowRight".into()),
            Key::Home => Some("Home".into()),
            Key::End => Some("End".into()),
            Key::PageUp => Some("PageUp".into()),
            Key::PageDown => Some("PageDown".into()),
            Key::CapsLock => Some("CapsLock".into()),
            Key::Delete => Some("Delete".into()),
            Key::Insert => Some("Insert".into()),
            Key::F1 => Some("F1".into()),
            Key::F2 => Some("F2".into()),
            Key::F3 => Some("F3".into()),
            Key::F4 => Some("F4".into()),
            Key::F5 => Some("F5".into()),
            Key::F6 => Some("F6".into()),
            Key::F7 => Some("F7".into()),
            Key::F8 => Some("F8".into()),
            Key::F9 => Some("F9".into()),
            Key::F10 => Some("F10".into()),
            Key::F11 => Some("F11".into()),
            Key::F12 => Some("F12".into()),
            Key::Num0 => Some("0".into()),
            Key::Num1 => Some("1".into()),
            Key::Num2 => Some("2".into()),
            Key::Num3 => Some("3".into()),
            Key::Num4 => Some("4".into()),
            Key::Num5 => Some("5".into()),
            Key::Num6 => Some("6".into()),
            Key::Num7 => Some("7".into()),
            Key::Num8 => Some("8".into()),
            Key::Num9 => Some("9".into()),
            Key::KeyA => Some("A".into()),
            Key::KeyB => Some("B".into()),
            Key::KeyC => Some("C".into()),
            Key::KeyD => Some("D".into()),
            Key::KeyE => Some("E".into()),
            Key::KeyF => Some("F".into()),
            Key::KeyG => Some("G".into()),
            Key::KeyH => Some("H".into()),
            Key::KeyI => Some("I".into()),
            Key::KeyJ => Some("J".into()),
            Key::KeyK => Some("K".into()),
            Key::KeyL => Some("L".into()),
            Key::KeyM => Some("M".into()),
            Key::KeyN => Some("N".into()),
            Key::KeyO => Some("O".into()),
            Key::KeyP => Some("P".into()),
            Key::KeyQ => Some("Q".into()),
            Key::KeyR => Some("R".into()),
            Key::KeyS => Some("S".into()),
            Key::KeyT => Some("T".into()),
            Key::KeyU => Some("U".into()),
            Key::KeyV => Some("V".into()),
            Key::KeyW => Some("W".into()),
            Key::KeyX => Some("X".into()),
            Key::KeyY => Some("Y".into()),
            Key::KeyZ => Some("Z".into()),
            Key::Minus => Some("-".into()),
            Key::Equal => Some("=".into()),
            Key::LeftBracket => Some("[".into()),
            Key::RightBracket => Some("]".into()),
            Key::SemiColon => Some(";".into()),
            Key::Quote => Some("'".into()),
            Key::Comma => Some(",".into()),
            Key::Dot => Some(".".into()),
            Key::Slash => Some("/".into()),
            Key::BackSlash => Some("\\".into()),
            Key::BackQuote => Some("`".into()),
            Key::Function => Some("Fn".into()),
            Key::PrintScreen => Some("PrintScreen".into()),
            Key::ScrollLock => Some("ScrollLock".into()),
            Key::Pause => Some("Pause".into()),
            Key::NumLock => Some("NumLock".into()),
            Key::KpDivide => Some("Kp/".into()),
            Key::KpMultiply => Some("Kp*".into()),
            Key::KpMinus => Some("Kp-".into()),
            Key::KpPlus => Some("Kp+".into()),
            Key::KpReturn => Some("KpEnter".into()),
            Key::Kp0 => Some("NumPad0".into()),
            Key::Kp1 => Some("NumPad1".into()),
            Key::Kp2 => Some("NumPad2".into()),
            Key::Kp3 => Some("NumPad3".into()),
            Key::Kp4 => Some("NumPad4".into()),
            Key::Kp5 => Some("NumPad5".into()),
            Key::Kp6 => Some("NumPad6".into()),
            Key::Kp7 => Some("NumPad7".into()),
            Key::Kp8 => Some("NumPad8".into()),
            Key::Kp9 => Some("NumPad9".into()),
            Key::KpDelete => Some("NumPadDel".into()),
            Key::Unknown(code) => Some(format!("Keycode({code})")),
            _ => None,
        }
    }

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    fn build_combination(modifiers: &HashSet<Key>, primary: Key) -> Option<String> {
        let mut parts = Vec::new();
        if modifiers.contains(&Key::ControlLeft) || modifiers.contains(&Key::ControlRight) {
            parts.push("Ctrl".to_string());
        }
        if modifiers.contains(&Key::Alt) || modifiers.contains(&Key::AltGr) {
            parts.push("Alt".to_string());
        }
        if modifiers.contains(&Key::ShiftLeft) || modifiers.contains(&Key::ShiftRight) {
            parts.push("Shift".to_string());
        }
        if modifiers.contains(&Key::MetaLeft) || modifiers.contains(&Key::MetaRight) {
            #[cfg(target_os = "macos")]
            let label = "Cmd";
            #[cfg(not(target_os = "macos"))]
            let label = "Win";
            parts.push(label.to_string());
        }

        if let Some(primary_label) = Self::describe_key(primary) {
            parts.push(primary_label);
            Some(parts.join("+"))
        } else {
            None
        }
    }
}

#[cfg_attr(not(any(target_os = "macos", target_os = "windows")), allow(dead_code))]
enum CaptureSignal {
    Captured(String),
    Error(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    use filetime::FileTime;

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
    fn onboarding_preferences_are_signed_and_verified() {
        let temp = tempdir().expect("tempdir");
        let config_path = temp.path().join("hotkey.json");
        let key = sample_key(31);
        let state = AppState::new(config_path.clone(), key.clone(), HotkeyBinding::default());

        state
            .update_permission_status("microphone", true)
            .expect("permission update should persist");

        drop(state);

        let onboarding_path = config_path.parent().unwrap().join("onboarding.json");
        let raw = fs::read(&onboarding_path).expect("onboarding file should exist");
        let envelope: OnboardingConfigEnvelope =
            serde_json::from_slice(&raw).expect("onboarding file should be valid JSON");

        let verified = verify_onboarding_envelope(&key, envelope.clone())
            .expect("signature should verify for untampered payload");
        assert!(
            verified.permissions.microphone,
            "permission flag should be restored from verified payload"
        );

        let mut tampered = envelope;
        tampered.payload.permissions.microphone = false;
        assert!(
            verify_onboarding_envelope(&key, tampered).is_err(),
            "tampered onboarding payload should be rejected"
        );
    }

    #[test]
    fn device_sample_roundtrip_is_sealed() {
        let temp = tempdir().expect("tempdir");
        let config_path = temp.path().join("hotkey.json");
        let key = sample_key(7);
        let state = AppState::new(config_path.clone(), key.clone(), HotkeyBinding::default());

        let payload = vec![1u8, 2, 3, 4, 5, 6, 7, 8];
        let sealed = state
            .store_device_sample("device-test-sealed", &payload)
            .expect("storing device sample should succeed");

        let restored = state
            .load_device_sample(&sealed.token)
            .expect("loading device sample should succeed");

        assert_eq!(restored, payload);
    }

    #[test]
    fn device_sample_detects_tampering() {
        let temp = tempdir().expect("tempdir");
        let config_path = temp.path().join("hotkey.json");
        let key = sample_key(11);
        let state = AppState::new(config_path.clone(), key.clone(), HotkeyBinding::default());

        let payload = vec![9u8, 8, 7, 6, 5, 4, 3, 2];
        let sealed = state
            .store_device_sample("device-test-tamper", &payload)
            .expect("storing device sample should succeed");

        let raw = fs::read(&sealed.path).expect("sample envelope should exist");
        let mut envelope: SampleEnvelope =
            serde_json::from_slice(&raw).expect("envelope should decode");
        let mut ciphertext = super::BASE64
            .decode(envelope.ciphertext.as_bytes())
            .expect("ciphertext should decode");
        ciphertext[0] ^= 0xAA;
        envelope.ciphertext = super::BASE64.encode(ciphertext);
        let encoded = serde_json::to_vec(&envelope).expect("re-encoding envelope");
        fs::write(&sealed.path, encoded).expect("writing tampered envelope");

        let result = state.load_device_sample(&sealed.token);
        assert!(result.is_err(), "tampering must be detected");
    }

    #[test]
    fn sample_cleanup_removes_expired_entries() {
        let temp = tempdir().expect("tempdir");
        let config_path = temp.path().join("hotkey.json");
        let key = sample_key(12);
        let state = AppState::new(config_path.clone(), key, HotkeyBinding::default());

        let payload = vec![0xAB; 32];
        let sealed = state
            .store_device_sample("expired", &payload)
            .expect("store expired sample");

        let past = SystemTime::now()
            .checked_sub(Duration::from_secs(SAMPLE_RETENTION_SECS + 60))
            .expect("timestamp should underflow safely");
        let ft = FileTime::from_system_time(past);
        filetime::set_file_mtime(&sealed.path, ft).expect("set past mtime");

        let stats = state.cleanup_samples().expect("cleanup succeeds");
        assert!(stats.removed >= 1, "expired sample should be removed");
        assert!(!sealed.path.exists(), "expired sample is deleted");
    }

    #[test]
    fn sample_cleanup_limits_capacity() {
        let temp = tempdir().expect("tempdir");
        let config_path = temp.path().join("hotkey.json");
        let key = sample_key(13);
        let state = AppState::new(config_path.clone(), key, HotkeyBinding::default());

        for idx in 0..(SAMPLE_RETENTION_CAPACITY + 3) {
            let token = format!("sample-{idx}");
            let payload = vec![idx as u8; 16];
            let sealed = state
                .store_device_sample(&token, &payload)
                .expect("store sample");

            let offset = (SAMPLE_RETENTION_CAPACITY + 3 - idx) as u64;
            let ts = SystemTime::now()
                .checked_sub(Duration::from_secs(offset))
                .unwrap_or(SystemTime::now());
            let ft = FileTime::from_system_time(ts);
            filetime::set_file_mtime(&sealed.path, ft).expect("set ordered mtime");
        }

        let stats = state.cleanup_samples().expect("cleanup succeeds");
        assert!(
            stats.retained <= SAMPLE_RETENTION_CAPACITY,
            "sample directory should respect retention capacity"
        );

        let mut count = 0;
        for entry in fs::read_dir(state.sample_dir()).expect("sample dir") {
            let entry = entry.expect("entry");
            if entry.path().extension().and_then(|ext| ext.to_str()) == Some("json") {
                count += 1;
            }
        }

        assert!(count <= SAMPLE_RETENTION_CAPACITY);
    }

    #[cfg(unix)]
    #[test]
    fn sample_cleanup_reports_errors_when_deletion_fails() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempdir().expect("tempdir");
        let config_path = temp.path().join("hotkey.json");
        let key = sample_key(14);
        let state = AppState::new(config_path.clone(), key, HotkeyBinding::default());

        let payload = vec![0xCD; 16];
        let sealed = state
            .store_device_sample("protected", &payload)
            .expect("store protected sample");

        let past = SystemTime::now()
            .checked_sub(Duration::from_secs(SAMPLE_RETENTION_SECS + 60))
            .expect("timestamp should underflow safely");
        let ft = FileTime::from_system_time(past);
        filetime::set_file_mtime(&sealed.path, ft).expect("set past mtime");

        fs::set_permissions(state.sample_dir(), fs::Permissions::from_mode(0o500))
            .expect("restrict directory");

        let stats = state.cleanup_samples().expect("cleanup executes");
        assert!(
            stats.errors >= 1,
            "cleanup should report removal errors when directory is read-only"
        );
        assert!(sealed.path.exists(), "sample remains when deletion fails");

        fs::set_permissions(state.sample_dir(), fs::Permissions::from_mode(0o700))
            .expect("restore permissions");
    }

    #[test]
    fn audio_cache_keys_are_derived_deterministically() {
        let master = sample_key(21);
        let keys_a = derive_audio_cache_keys(&master).expect("derive keys");
        let keys_b = derive_audio_cache_keys(&master).expect("derive keys again");
        assert_eq!(keys_a, keys_b, "derivation should be deterministic");

        let other_master = sample_key(22);
        let keys_c = derive_audio_cache_keys(&other_master).expect("derive other keys");
        assert_ne!(keys_a, keys_c, "different masters must yield distinct keys");
    }

    #[test]
    fn device_sample_cannot_be_opened_with_different_audio_key() {
        let temp = tempdir().expect("tempdir");
        let config_path = temp.path().join("hotkey.json");
        let key = sample_key(9);
        let state = AppState::new(config_path.clone(), key.clone(), HotkeyBinding::default());

        let payload = vec![5u8; 160];
        state
            .store_device_sample("device-test", &payload)
            .expect("sealing succeeds");

        drop(state);

        let wrong_key = sample_key(10);
        let restored = AppState::new(config_path.clone(), wrong_key, HotkeyBinding::default());
        let result = restored.load_device_sample("device-test");
        assert!(
            result.is_err(),
            "audio samples must not decrypt with a different derived key"
        );
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

    #[test]
    fn onboarding_preferences_persist_between_sessions() {
        let temp = tempdir().expect("tempdir");
        let config_path = temp.path().join("hotkey.json");
        let key = sample_key(6);
        let state = AppState::new(config_path.clone(), key.clone(), HotkeyBinding::default());

        state
            .update_engine_choice("hybrid")
            .expect("engine choice should persist");
        state
            .save_calibration(
                "device::1",
                SavedCalibration {
                    threshold: 0.55,
                    noise_floor_db: -42.0,
                    sample_window_ms: 5000,
                    device_label: Some("Mock".into()),
                    mode: CalibrationMode::Manual,
                    recommended_threshold: Some(0.62),
                    updated_at_ms: None,
                    noise_alert: false,
                    noise_hint: None,
                    strong_noise_mode: false,
                    frame_window_ms: None,
                },
            )
            .expect("calibration persistence");
        state
            .mark_tutorial_complete()
            .expect("tutorial flag persists");

        drop(state);

        let restored = AppState::new(config_path, key, HotkeyBinding::default());
        assert_eq!(
            restored.engine_choice().as_deref(),
            Some("hybrid"),
            "engine choice should be restored"
        );
        let calibration = restored
            .calibration_for("device::1")
            .expect("calibration restored");
        assert!((calibration.threshold - 0.55).abs() < f32::EPSILON);
        assert_eq!(calibration.mode, CalibrationMode::Manual);
        assert_eq!(calibration.recommended_threshold, Some(0.62));
        assert!(calibration.updated_at_ms.is_some());
        assert!(
            restored.tutorial_completed(),
            "tutorial completion restored"
        );
        assert_eq!(
            restored.tutorial_status(),
            Some(TutorialStatus::Completed),
            "tutorial status should record completion"
        );
    }

    #[test]
    fn tutorial_skip_is_recorded_and_restored() {
        let temp = tempdir().expect("tempdir");
        let config_path = temp.path().join("hotkey.json");
        let key = sample_key(7);
        let state = AppState::new(config_path.clone(), key.clone(), HotkeyBinding::default());

        state.mark_tutorial_skipped().expect("skip flag persists");

        drop(state);

        let restored = AppState::new(config_path, key, HotkeyBinding::default());
        assert!(
            restored.tutorial_completed(),
            "skipping should still satisfy onboarding requirements"
        );
        assert_eq!(
            restored.tutorial_status(),
            Some(TutorialStatus::Skipped),
            "tutorial status should indicate skipped"
        );
    }
    #[test]
    fn permission_status_roundtrip_and_validation() {
        let temp = tempdir().expect("tempdir");
        let config_path = temp.path().join("hotkey.json");
        let key = sample_key(7);
        let state = AppState::new(config_path.clone(), key.clone(), HotkeyBinding::default());

        let initial = state.permission_status();
        assert!(!initial.microphone);
        assert!(!initial.accessibility);

        state
            .update_permission_status("microphone", true)
            .expect("microphone flag persists");
        state
            .update_permission_status("accessibility", false)
            .expect("accessibility flag persists");

        drop(state);

        let restored = AppState::new(config_path.clone(), key, HotkeyBinding::default());
        let status = restored.permission_status();
        assert!(status.microphone);
        assert!(!status.accessibility);

        let err = restored.update_permission_status("unknown", true);
        assert!(err.is_err(), "unknown permission keys should be rejected");
    }

    #[test]
    fn selected_microphone_persists_and_can_be_cleared() {
        let temp = tempdir().expect("tempdir");
        let config_path = temp.path().join("hotkey.json");
        let key = sample_key(8);
        let state = AppState::new(config_path.clone(), key.clone(), HotkeyBinding::default());

        assert!(state.selected_microphone().is_none());
        state
            .persist_selected_microphone(Some("device::primary".into()))
            .expect("selection should persist");
        assert_eq!(
            state.selected_microphone().as_deref(),
            Some("device::primary")
        );

        drop(state);

        let restored = AppState::new(config_path.clone(), key, HotkeyBinding::default());
        assert_eq!(
            restored.selected_microphone().as_deref(),
            Some("device::primary")
        );
        restored
            .persist_selected_microphone(None)
            .expect("selection can be cleared");
        assert!(restored.selected_microphone().is_none());
    }
}
