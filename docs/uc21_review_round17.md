# UC2.1 Review (Round 17)

## Context
Evaluate the latest realtime transcription stack against Sprint 2 UC2.1 and the architecture mandate that Whisper must deliver the first 400 ms feedback locally while cloud decoding only promotes fallback results after a proven local degradation.【F:docs/sprint/sprint2.md†L8-L11】【F:docs/architecture.md†L30-L33】

## Findings

1. **Fallback promotion lags the cadence timeout** — *Status: Completed*
   - Remediation: The cloud gate now records a timeout before leaving the wait loop, marks `LocalProgress` degraded, and notifies waiters ahead of publishing the fallback transcript. As a result, the first cloud diff that follows a missed 400 ms/200 ms window is emitted as `is_primary = true` alongside the degradation notice, aligning with UC2.1’s fallback expectations.【F:core/src/orchestrator/mod.rs†L722-L770】【F:core/src/orchestrator/mod.rs†L121-L210】【F:docs/sprint/sprint2.md†L8-L11】
   - Validation: `cargo test --manifest-path core/Cargo.toml`【499bb4†L1-L19】

2. **Cloud-preferred sessions still suppress Whisper-first primacy** — *Status: Completed*
   - Remediation: Local transcripts compute primacy using the degradation state instead of the session preference, keeping the Whisper diff marked primary until the local path reports a fault. This preserves the mandated Whisper-first preview even for cloud-preferred sessions while still allowing cloud promotion once degradation is recorded.【F:core/src/orchestrator/mod.rs†L647-L706】【F:docs/sprint/sprint2.md†L8-L11】【F:docs/architecture.md†L30-L33】
   - Validation: `cargo test --manifest-path core/Cargo.toml`【499bb4†L1-L19】

## Status
- Gap 25 (Fallback promotion lags cadence timeout) — *Completed*.
- Gap 26 (Cloud-preferred sessions suppress Whisper-first primacy) — *Completed*.
