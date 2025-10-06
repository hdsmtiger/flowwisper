# UC2.1 Review (Round 8)

## Context
This round evaluates the latest realtime transcription implementation against the Sprint 2 UC2.1 acceptance criteria, the PRD, and the architecture guidance after the round-seven fixes landed.

## Findings

1. **First-window watchdog still fires during normal silence** — *Status: Completed*
   - Resolution: The realtime monitor now arms its 400 ms deadline only after `LocalProgress` reports RMS-driven speech activity, sampling every 50 ms until speech is detected and measuring latency from the speech timestamp instead of session start. Silence therefore leaves the watchdog idle, and the first-window violation is emitted only when Whisper misses the SLA after speech begins.【F:core/src/orchestrator/mod.rs†L60-L206】
   - Validation: `cargo test --manifest-path core/Cargo.toml`

2. **Whisper wrapper still performs full decoding per frame** — *Status: Completed*
   - Resolution: The Whisper streaming state now keeps only a 240 ms tail plus the most recent pending audio, flushing once speech energy or stride thresholds are hit so each decode processes ≤ ~450 ms of PCM. The worker reuses the shared state, clears processed pending audio, and delivers incremental diffs without replaying the full session buffer.【F:core/src/orchestrator/mod.rs†L777-L940】
   - Validation: `cargo test --manifest-path core/Cargo.toml`

3. **Audio pipeline still lacks enforced 100–200 ms segmentation** — *Status: Completed*
   - Resolution: `AudioPipeline::push_pcm_frame` now buffers raw PCM, rechunking it into 100–200 ms slices per the 16 kHz sample rate, tagging each chunk with RMS/VAD metadata, and fanning it out via bounded queues so no frames are dropped. Downstream session handlers therefore receive only cadence-compliant buffers.【F:core/src/audio/mod.rs†L1-L155】【F:core/src/session/mod.rs†L49-L88】
   - Validation: `cargo test --manifest-path core/Cargo.toml`

## Next Steps
Track the open items above in the UC2.1 gap list and address them with targeted fixes and regression coverage to restore alignment with the sprint acceptance criteria.
