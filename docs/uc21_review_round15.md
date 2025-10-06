# UC2.1 Review (Round 15)

## Context
Evaluate the "per-frame Whisper-first gating" patch against Sprint 2 UC2.1's local-first requirement and the architecture's audio pipeline guarantees (100–200 ms frames, local-first transcripts, cloud fallback on degradation).【F:docs/sprint/sprint2.md†L8-L11】【F:docs/architecture.md†L92-L103】【F:docs/architecture.md†L187-L201】

## Findings

1. **Fallback transcripts stay secondary when Whisper misses the latency SLA** — *Status: Completed*
   - The watchdog now sets `LocalProgress` to degraded whenever the 400 ms first-window or rolling cadence timers fire, so the session is marked unhealthy as soon as Whisper misses its SLA.【F:core/src/orchestrator/mod.rs†L120-L207】 The cloud path continues to consult `LocalProgress` and promotes fallback transcripts (`is_primary = true`) whenever the local path is degraded, restoring UC2.1's "本地优先、异常时回退提示" behaviour.【F:core/src/orchestrator/mod.rs†L688-L736】【F:docs/sprint/sprint2.md†L8-L11】
   - **Validation:** `cargo test --manifest-path core/Cargo.toml`

2. **PCM shedding drops primary audio under backpressure** — *Status: Completed*
   - The audio pipeline exposes a lossless subscription path that blocks on backpressure instead of dropping frames, and the session manager now routes Whisper’s feed through that lossless channel so every 100–200 ms slice reaches the decoder.【F:core/src/audio/mod.rs†L23-L170】【F:core/src/session/mod.rs†L60-L96】 Other subscribers retain bounded queues with shedding, keeping auxiliary consumers isolated while protecting the primary feed.
   - **Validation:** `cargo test --manifest-path core/Cargo.toml`

## Status
- Gap 21 (Fallback transcripts stay secondary on latency misses) — *Completed*.
- Gap 22 (PCM shedding drops primary audio under backpressure) — *Completed*.
