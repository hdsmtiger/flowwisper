use super::*;
use anyhow::{anyhow, Context, Result as AnyhowResult};
use dirs::data_dir;
use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::mem::transmute;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tracing::{info, warn};
use whisper_rs::{
    FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperState,
};

use crate::orchestrator::constants::SPEECH_RMS_THRESHOLD;

const DEFAULT_MODEL_URL: &str =
    "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.en.bin";
const DEFAULT_MODEL_FILENAME: &str = "ggml-base.en.bin";

pub struct WhisperLocalEngine {
    _context: Arc<WhisperContext>,
    streaming: Arc<Mutex<StreamingState>>,
}

impl WhisperLocalEngine {
    pub fn from_env() -> Result<Self> {
        let model_path = resolve_or_fetch_model()?;
        Self::from_model_path(model_path)
    }

    pub fn from_model_path<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path_ref = path.as_ref();
        let path_str = path_ref
            .to_str()
            .ok_or_else(|| anyhow!("模型路径不是有效的 UTF-8"))?;
        let context = Arc::new(WhisperContext::new_with_params(
            path_str,
            WhisperContextParameters::default(),
        )?);
        let state = unsafe {
            transmute::<WhisperState<'_>, WhisperState<'static>>(context.create_state()?)
        };
        Ok(Self {
            _context: Arc::clone(&context),
            streaming: Arc::new(Mutex::new(StreamingState::new(state))),
        })
    }
}

fn resolve_or_fetch_model() -> AnyhowResult<PathBuf> {
    if let Ok(path) = std::env::var("WHISPER_MODEL_PATH") {
        let path_buf = PathBuf::from(path);
        if path_buf.is_file() {
            return Ok(path_buf);
        }
        return Err(anyhow!(
            "WHISPER_MODEL_PATH points to a missing file: {}",
            path_buf.display()
        ));
    }

    if !auto_download_enabled() {
        warn!(
            target: "engine_orchestrator",
            "WHISPER_MODEL_PATH missing and auto-download disabled"
        );
        return Err(anyhow!(
            "WHISPER_MODEL_PATH is not set and auto-download is disabled"
        ));
    }

    let cache_path = default_model_path()?;
    if cache_path.is_file() {
        std::env::set_var("WHISPER_MODEL_PATH", &cache_path);
        return Ok(cache_path);
    }

    let url = std::env::var("WHISPER_MODEL_URL").unwrap_or_else(|_| DEFAULT_MODEL_URL.into());
    download_model(&cache_path, &url)?;
    std::env::set_var("WHISPER_MODEL_PATH", &cache_path);
    Ok(cache_path)
}

fn default_model_path() -> AnyhowResult<PathBuf> {
    if let Ok(dir) = std::env::var("FLOWWISPER_MODEL_DIR") {
        let path = PathBuf::from(dir).join(DEFAULT_MODEL_FILENAME);
        ensure_parent_dir(&path)?;
        return Ok(path);
    }

    let base_dir = data_dir()
        .map(|dir| dir.join("Flowwisper").join("models"))
        .ok_or_else(|| anyhow!("failed to determine default model cache directory"))?;
    let path = base_dir.join(DEFAULT_MODEL_FILENAME);
    ensure_parent_dir(&path)?;
    Ok(path)
}

fn ensure_parent_dir(path: &Path) -> AnyhowResult<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| {
            format!(
                "failed to create whisper model directory: {}",
                parent.display()
            )
        })?;
    }
    Ok(())
}

fn download_model(path: &Path, url: &str) -> AnyhowResult<()> {
    info!(
        target: "engine_orchestrator",
        %url,
        path = %path.display(),
        "downloading whisper model"
    );

    let temp_path = path.with_extension("download");
    let response = ureq::get(url)
        .call()
        .map_err(|err| anyhow!("failed to download whisper model: {err}"))?;

    if !(200..300).contains(&response.status()) {
        return Err(anyhow!(
            "failed to download whisper model: received HTTP status {}",
            response.status()
        ));
    }

    let mut reader = response.into_reader();
    let file = File::create(&temp_path).with_context(|| {
        format!(
            "failed to create temporary whisper model file: {}",
            temp_path.display()
        )
    })?;
    let mut writer = BufWriter::new(file);

    std::io::copy(&mut reader, &mut writer)
        .with_context(|| format!("failed to write whisper model to {}", temp_path.display()))?;
    writer
        .flush()
        .context("failed to flush whisper model to disk")?;

    fs::rename(&temp_path, path).with_context(|| {
        format!(
            "failed to finalize whisper model download to {}",
            path.display()
        )
    })?;

    info!(
        target: "engine_orchestrator",
        path = %path.display(),
        "whisper model ready"
    );

    Ok(())
}

fn auto_download_enabled() -> bool {
    if let Ok(value) = std::env::var("WHISPER_DISABLE_AUTO_DOWNLOAD") {
        return !matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        );
    }

    if let Ok(value) = std::env::var("WHISPER_AUTO_DOWNLOAD") {
        return matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        );
    }

    true
}

fn suffix_prefix_overlap(existing: &str, new_text: &str) -> usize {
    let max = existing.len().min(new_text.len());
    for overlap in (1..=max).rev() {
        if existing.ends_with(&new_text[..overlap]) {
            return overlap;
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Mutex, OnceLock};
    use std::thread;
    use tempfile::tempdir;

    fn env_guard() -> &'static Mutex<()> {
        static GUARD: OnceLock<Mutex<()>> = OnceLock::new();
        GUARD.get_or_init(|| Mutex::new(()))
    }

    fn reset_env() {
        std::env::remove_var("FLOWWISPER_MODEL_DIR");
        std::env::remove_var("WHISPER_MODEL_PATH");
        std::env::remove_var("WHISPER_MODEL_URL");
        std::env::remove_var("WHISPER_AUTO_DOWNLOAD");
        std::env::remove_var("WHISPER_DISABLE_AUTO_DOWNLOAD");
    }

    #[test]
    fn downloads_model_when_missing() {
        let _lock = env_guard().lock().expect("env guard poisoned");
        reset_env();

        let directory = tempdir().expect("tempdir should create model cache");
        std::env::set_var("FLOWWISPER_MODEL_DIR", directory.path());

        let payload = b"fake-whisper-model".to_vec();
        let expected = payload.clone();

        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind ephemeral port");
        let address = listener.local_addr().expect("local addr available");
        let handle = thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let mut buffer = [0_u8; 512];
                let _ = stream.read(&mut buffer);
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    payload.len()
                );
                stream
                    .write_all(response.as_bytes())
                    .expect("response headers written");
                stream.write_all(&payload).expect("response body written");
            }
        });

        std::env::set_var(
            "WHISPER_MODEL_URL",
            format!("http://{address}/{}", DEFAULT_MODEL_FILENAME),
        );

        let model_path = resolve_or_fetch_model().expect("model should download");
        handle.join().expect("http server thread joined");

        assert_eq!(std::fs::read(&model_path).expect("read model"), expected);
        assert_eq!(
            std::env::var("WHISPER_MODEL_PATH").expect("model path exported"),
            model_path.to_string_lossy()
        );

        reset_env();
    }

    #[test]
    fn reuses_cached_model_when_present() {
        let _lock = env_guard().lock().expect("env guard poisoned");
        reset_env();

        let directory = tempdir().expect("tempdir should create model cache");
        let cached_path = directory.path().join(DEFAULT_MODEL_FILENAME);
        std::fs::write(&cached_path, b"existing-model").expect("write cached whisper model");

        std::env::set_var("FLOWWISPER_MODEL_DIR", directory.path());
        std::env::set_var("WHISPER_MODEL_URL", "http://127.0.0.1:1/never-used");

        let model_path = resolve_or_fetch_model().expect("should reuse cached model");

        assert_eq!(model_path, cached_path);
        assert_eq!(
            std::fs::read(&model_path).expect("read cached model"),
            b"existing-model"
        );
        assert_eq!(
            std::env::var("WHISPER_MODEL_PATH").expect("model path exported"),
            model_path.to_string_lossy()
        );

        reset_env();
    }
}

struct StreamingState {
    state: WhisperState<'static>,
    tail: Vec<f32>,
    pending: Vec<f32>,
    emitted: String,
    lookback_samples: usize,
    sample_rate: usize,
    min_stride_samples: usize,
    max_stride_samples: usize,
}

impl StreamingState {
    fn new(state: WhisperState<'static>) -> Self {
        const SAMPLE_RATE: usize = 16_000;
        const LOOKBACK_MS: usize = 240;
        const MIN_STRIDE_MS: usize = 80;
        const MAX_STRIDE_MS: usize = 200;
        let lookback_samples = (SAMPLE_RATE * LOOKBACK_MS + 999) / 1_000;
        let min_stride_samples = (SAMPLE_RATE * MIN_STRIDE_MS + 999) / 1_000;
        let max_stride_samples = (SAMPLE_RATE * MAX_STRIDE_MS + 999) / 1_000;
        Self {
            state,
            tail: Vec::with_capacity(lookback_samples),
            pending: Vec::with_capacity(max_stride_samples),
            emitted: String::new(),
            lookback_samples,
            sample_rate: SAMPLE_RATE,
            min_stride_samples,
            max_stride_samples,
        }
    }
}

#[async_trait]
impl SpeechEngine for WhisperLocalEngine {
    async fn transcribe(&self, frame: &[f32]) -> Result<String> {
        if frame.is_empty() {
            return Ok(String::new());
        }

        let pcm: Vec<f32> = frame.to_vec();
        let speechy = frame_rms(frame) >= SPEECH_RMS_THRESHOLD;
        let streaming = Arc::clone(&self.streaming);

        tokio::task::spawn_blocking(move || {
            let mut guard = streaming
                .lock()
                .expect("whisper streaming state lock poisoned");

            guard.pending.extend_from_slice(&pcm);

            let should_decode = if speechy {
                guard.pending.len() >= guard.min_stride_samples
            } else {
                guard.pending.len() >= guard.max_stride_samples
            };

            if !should_decode {
                return Ok(String::new());
            }

            let mut decode_window = Vec::with_capacity(guard.tail.len() + guard.pending.len());
            decode_window.extend_from_slice(&guard.tail);
            decode_window.extend_from_slice(&guard.pending);

            if decode_window.is_empty() {
                return Ok(String::new());
            }

            let mut params = FullParams::new(SamplingStrategy::default());
            params.set_translate(false);
            params.set_single_segment(true);
            params.set_temperature(0.0);
            params.set_no_context(false);
            params.set_print_realtime(false);
            params.set_print_progress(false);

            let duration_ms = ((decode_window.len() * 1_000) / guard.sample_rate).max(1) as i32;
            params.set_duration_ms(duration_ms);

            guard.state.full(params, &decode_window)?;
            guard.pending.clear();

            let tail_len = guard.lookback_samples.min(decode_window.len());
            guard.tail = decode_window[decode_window.len() - tail_len..].to_vec();

            let mut transcript = String::new();
            let segments = guard.state.full_n_segments()? as usize;
            for segment in 0..segments {
                let text = guard.state.full_get_segment_text(segment as i32)?;
                let trimmed = text.trim();
                if trimmed.is_empty() {
                    continue;
                }
                if !transcript.is_empty() {
                    transcript.push(' ');
                }
                transcript.push_str(trimmed);
            }

            let transcript = transcript.trim().to_string();
            if transcript.is_empty() {
                return Ok(String::new());
            }

            let overlap = suffix_prefix_overlap(&guard.emitted, &transcript);
            let mut delta = transcript[overlap..].trim_start().to_string();

            if delta.is_empty() && guard.emitted.contains(&transcript) {
                return Ok(String::new());
            }

            if !delta.is_empty() {
                if !guard.emitted.is_empty()
                    && !guard.emitted.ends_with(' ')
                    && !delta.starts_with(' ')
                {
                    guard.emitted.push(' ');
                }
                guard.emitted.push_str(&delta);
            } else {
                guard.emitted = transcript.clone();
                delta = transcript;
            }

            Ok(delta)
        })
        .await?
    }
}
