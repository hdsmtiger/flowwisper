# UC2.1 Review (Round 20)

## Context
Re-assessed the realtime audio/orchestration stack after the round-19 fixes to verify UC2.1 still meets the 400 ms Whisper-first feedback, 200 ms cadence, and fallback-notice expectations called out in Sprint 2 and the architecture brief.【F:docs/sprint/sprint2.md†L8-L11】【F:docs/architecture.md†L87-L105】

## Findings

1. **Cloud gate trips during silence** — *Status: Completed*
   - Remediation: The cloud gating loop now checks `LocalProgress::has_speech_started()` and `is_speech_active()` before tripping the deadline, so watchdog degradation and fallback notices only trigger once Whisper misses the SLA with active speech present.【F:core/src/orchestrator/mod.rs†L803-L855】【F:docs/sprint/sprint2.md†L8-L11】【F:docs/architecture.md†L87-L105】
   - Validation: `cargo test --manifest-path core/Cargo.toml`【e63d5b†L1-L5】

2. **Waveform telemetry bursts instead of 30–60 fps pacing** — *Status: Completed*
   - Remediation: Added a background waveform scheduler that emits 32 ms frames on a steady ticker, feeds frames from a rolling queue, pads tail slices, and generates a silence pre-roll before the first PCM batch so the UI receives continuous 30–60 fps telemetry.【F:core/src/audio/mod.rs†L23-L204】【F:core/src/audio/mod.rs†L330-L360】【F:docs/architecture.md†L87-L105】
   - Validation: `cargo test --manifest-path core/Cargo.toml`【e63d5b†L1-L5】

## Status
- Gap 31 (Cloud gate trips during silence) — *Completed*.
- Gap 32 (Waveform telemetry bursts instead of 30–60 fps pacing) — *Completed*.
