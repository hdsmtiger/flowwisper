//! 音频采集与处理管线脚手架。

use anyhow::Result;
use bytes::Bytes;
use tokio::sync::broadcast;
use tracing::info;

pub struct AudioPipeline {
    waveform_tx: broadcast::Sender<WaveformFrame>,
}

#[derive(Clone, Debug)]
pub struct WaveformFrame {
    pub rms: f32,
    pub vad_active: bool,
}

impl AudioPipeline {
    pub fn new() -> Self {
        let (waveform_tx, _) = broadcast::channel(32);
        Self { waveform_tx }
    }

    pub fn subscribe_waveform(&self) -> broadcast::Receiver<WaveformFrame> {
        self.waveform_tx.subscribe()
    }

    pub async fn start(&self) -> Result<()> {
        info!(target: "audio_pipeline", "starting placeholder pipeline");
        Ok(())
    }

    pub async fn handle_frame(&self, _pcm: Bytes) -> Result<()> {
        // TODO: 接入 CoreAudio/WASAPI 并执行 VAD、降噪、增益控制。
        Ok(())
    }
}
