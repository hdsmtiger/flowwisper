# UC2.1 Review (Round 19)

## Context
Re-evaluate the realtime transcription stack after the "whisper primacy & waveform cadence" changes to ensure UC2.1 still meets the 400 ms local-first feedback, 200 ms cadence, and fallback-notice requirements from Sprint 2 and the architecture brief.【F:docs/sprint/sprint2.md†L8-L11】【F:docs/architecture.md†L87-L104】

## Findings

1. **Local send failures drop the fallback notice** — *Status: Completed*
   - Remediation: The realtime worker now emits a WARN-level `SessionNotice` whenever the local transcript send fails, using the same fallback messaging as other latency breaches so the session manager can surface the prompt even when Whisper backpressures.【F:core/src/orchestrator/mod.rs†L706-L727】【F:docs/sprint/sprint2.md†L8-L11】
   - Validation: `cargo test --manifest-path core/Cargo.toml`【e8a4e1†L1-L23】

2. **Cadence timeouts promote cloud silently** — *Status: Completed*
   - Remediation: The cloud gating path now publishes the WARN-level fallback notice before releasing degraded cloud transcripts, ensuring every cadence miss raises the mandated rollback prompt even when the watchdog sees the degraded flag.【F:core/src/orchestrator/mod.rs†L812-L834】【F:core/src/orchestrator/mod.rs†L1520-L1583】【F:docs/sprint/sprint2.md†L8-L11】
   - Validation: `cargo test --manifest-path core/Cargo.toml`【e8a4e1†L1-L23】

## Status
- Gap 29 (Local send failures drop the fallback notice) — *Completed*.
- Gap 30 (Cadence timeouts promote cloud silently) — *Completed*.
