# UC2.1 Review (Round 14)

## Context
Evaluate the latest realtime orchestrator changes against Sprint 2 UC2.1’s mandate that Whisper drive the first 400 ms response and 200 ms cadence while cloud decoding stays a fallback within the architecture’s local-first routing plan.【F:docs/sprint/sprint2.md†L8-L10】【F:docs/architecture.md†L30-L31】【F:docs/architecture.md†L145-L149】

## Findings

1. **Cloud gating collapses after the first Whisper diff** — *Completed*
   - **Remediation:** Cloud workers now consult `LocalProgress` for every frame and remain in the wait loop until the matching local frame succeeds, degrades, or times out, so later cloud updates never overtake Whisper even in cloud-preferred sessions.【F:core/src/orchestrator/mod.rs†L713-L744】 Regression coverage in `cloud_waits_for_local_each_frame` and the new `cloud_preferred_waits_for_whisper_each_frame` verifies the per-frame gating across both strategy modes.【F:core/src/orchestrator/mod.rs†L1338-L1374】【F:core/src/orchestrator/mod.rs†L1460-L1519】
   - **Validation:** `cargo test --manifest-path core/Cargo.toml`

2. **Cloud fallback never becomes primary after local failure** — *Completed*
   - **Remediation:** When the local path marks a degradation, cloud transcripts are promoted by setting `is_primary = true`, ensuring downstream consumers receive the fallback result once Whisper fails and until it recovers.【F:core/src/orchestrator/mod.rs†L688-L736】 The `emits_notice_when_local_engine_fails` regression now confirms cloud promotion in degraded sessions.【F:core/src/orchestrator/mod.rs†L1266-L1303】
   - **Validation:** `cargo test --manifest-path core/Cargo.toml`

## Status
- Gap 19 (Cloud gating collapses after the first Whisper diff) — *Completed*.
- Gap 20 (Cloud fallback never becomes primary after local failure) — *Completed*.
