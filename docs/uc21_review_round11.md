# UC2.1 Review (Round 11)

## Context
Evaluate the post–round-ten realtime transcription stack against Sprint 2 UC2.1 and the architectural mandate that Whisper must deliver the first characters locally within 400 ms while the audio pipeline maintains 100–200 ms frame delivery. 【F:docs/sprint/sprint2.md†L8-L11】【F:docs/architecture.md†L28-L33】【F:docs/architecture.md†L96-L103】

## Findings

1. **Cloud-preferred sessions still preempt Whisper’s first characters** — *Status: Completed*
   - **Resolution:** The cloud worker now gates on the Whisper-first flag even when `prefer_cloud = true`, holding publication until the local path succeeds, degrades, or misses its deadline so the first user-visible diff continues to originate from Whisper. A new regression covers the case where the cloud engine is faster than Whisper while still verifying the first transcript is local.【F:core/src/orchestrator/mod.rs†L719-L787】【F:core/src/orchestrator/mod.rs†L1342-L1385】
   - **Validation:** `cargo test --manifest-path core/Cargo.toml` (covers `cloud_preferred_sessions_emit_local_first`).

2. **PCM accumulator drops the trailing sub-100 ms audio slice** — *Status: Completed*
   - **Resolution:** Introduced `AudioPipeline::flush_pending`, padding and emitting any residual samples below the 100 ms threshold and wiring the session manager to invoke it when PCM delivery stops so Whisper receives the full recording tail.【F:core/src/audio/mod.rs†L73-L120】【F:core/src/session/mod.rs†L57-L84】
   - **Validation:** `cargo test --manifest-path core/Cargo.toml` (covers `flushes_pending_tail_on_request`).

## Status
- Gap 14 (cloud-first preemption) — *Completed*.
- Gap 15 (PCM accumulator flush) — *Completed*.
