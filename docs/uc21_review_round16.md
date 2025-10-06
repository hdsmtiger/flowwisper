# UC2.1 Review (Round 16)

## Context
Evaluate the latest realtime orchestration patch against Sprint 2 UC2.1, focusing on the "本地优先" streaming guarantees and 100–200 ms cadence promised in the architecture.【F:docs/sprint/sprint2.md†L8-L11】【F:docs/architecture.md†L92-L103】【F:docs/architecture.md†L187-L201】

## Findings

1. **Local progress regresses on out-of-order completions** — *Status: Completed*
   - `LocalProgress::record_success` now uses a compare-and-swap loop so `last_frame` only advances when the new frame index exceeds the recorded value, preventing late completions from rewinding session progress and keeping the cadence watchdog aligned with delivered 100–200 ms increments.【F:core/src/orchestrator/mod.rs†L329-L350】【F:docs/sprint/sprint2.md†L8-L11】
   - **Validation:** `cargo test --manifest-path core/Cargo.toml`

2. **Local transcripts can publish out of order** — *Status: Completed*
   - Realtime local decoding is now serialized through a mutex-backed critical section, ensuring each Whisper frame is transcribed and published in the order it was enqueued so the cloud gate only opens after preceding local increments are committed.【F:core/src/orchestrator/mod.rs†L567-L705】【F:docs/architecture.md†L92-L103】【F:docs/architecture.md†L187-L201】
   - **Validation:** `cargo test --manifest-path core/Cargo.toml`

## Status
- Gap 23 (Local progress regresses on out-of-order completions) — *Completed*.
- Gap 24 (Local transcripts can publish out of order) — *Completed*.
