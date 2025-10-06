# UC2.1 Review (Round 9)

## Context
Evaluate the latest realtime transcription changes against Sprint 2 UC2.1, the PRD, and the architecture guidance after the round-eight fixes were merged.

## Findings

1. **Rolling cadence watchdog still fires during natural silence** — *Status: Completed*
   - Resolution: `LocalProgress` now records per-frame RMS and toggles a speech-active flag so the rolling watchdog only evaluates cadence while voice activity is present, eliminating false "本地解码增量延迟" notices during natural pauses.【F:core/src/orchestrator/mod.rs†L90-L205】【F:core/src/orchestrator/mod.rs†L304-L372】
   - Validation: `cargo test --manifest-path core/Cargo.toml` (covers `silence_does_not_trigger_cadence_notice`).

2. **Audio fan-out allows slow consumers to stall the realtime path** — *Status: Completed*
   - Resolution: `AudioPipeline::emit_chunk` now uses non-blocking `try_send` with background tasks for back-pressured subscribers so slow listeners cannot hold up the orchestrator’s 100–200 ms delivery cadence, and the PCM accumulator was upgraded to a `VecDeque` to maintain lossless 100–200 ms rechunking without quadratic drain costs under sustained load.【F:core/src/audio/mod.rs†L18-L116】
   - Validation: `cargo test --manifest-path core/Cargo.toml` (covers `slow_subscriber_does_not_block_realtime_feed`).

## Next Steps
Round-nine findings have been remediated and validated; keep the watchdog and audio fan-out suites in CI to guard against regressions.
