# UC2.1 Review (Round 18)

## Context
Evaluate the post-round-17 realtime stack against Sprint 2 UC2.1's "本地优先、400ms 首字" goal and the architecture's waveform telemetry mandate (30–60 fps) to confirm the recent regressions are resolved.【F:docs/sprint/sprint2.md†L8-L11】【F:docs/architecture.md†L28-L33】【F:docs/architecture.md†L92-L99】

## Findings

1. **Cloud promotion still overrides Whisper while healthy** — *Status: Completed*
   - Remediation: Local transcripts now tag primacy exclusively through `LocalProgress::is_degraded()`, keeping every Whisper diff primary until the watchdog or decoder marks a degradation, while the cloud path only promotes to primary once that degraded flag is set. The regression suite asserts that even in `prefer_cloud = true` sessions the cloud stream stays secondary while Whisper is healthy.【F:core/src/orchestrator/mod.rs†L666-L736】【F:core/src/orchestrator/mod.rs†L800-L878】【F:core/src/orchestrator/mod.rs†L1549-L1680】
   - Validation: `cargo test --manifest-path core/Cargo.toml`【17902e†L1-L24】

2. **Waveform bridge still runs at ASR cadence** — *Status: Completed*
   - Remediation: The audio pipeline now buffers PCM for the Whisper cadence while draining a parallel waveform accumulator that emits RMS/VAD telemetry every ~32 ms (≈31 fps) and flushes the trailing window on shutdown, decoupling waveform updates from ASR chunking as required by the telemetry bridge design.【F:core/src/audio/mod.rs†L13-L208】【F:core/src/audio/mod.rs†L344-L418】
   - Validation: `cargo test --manifest-path core/Cargo.toml`【17902e†L1-L24】

## Status
- Gap 27 (Cloud promotion still overrides Whisper while healthy) — *Completed*.
- Gap 28 (Waveform bridge still runs at ASR cadence) — *Completed*.
