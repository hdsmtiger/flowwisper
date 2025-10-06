# UC2.1 Review (Round 22)

## Context
Revalidated the realtime transcription stack against Sprint 2 UC2.1 after the round-21 fixes, focusing on whether the local-first experience, fallback notices, and Whisper integration still satisfy the PRD and architecture contracts.【F:docs/sprint/sprint2.md†L8-L11】【F:docs/architecture.md†L87-L112】

## Findings

1. **Dropped realtime notices for slow session clients** — *Status: Completed*
   - Remediation: `SessionManager::start_realtime_transcription` now guarantees WARN/Error notices by blocking on `send` when the client queue is saturated while continuing to drop only best-effort transcript diffs, so UC2.1’s rollback prompts always reach slow consumers without stalling the broadcast bus.【F:core/src/session/mod.rs†L107-L148】
   - Validation: `delivers_warn_notice_to_slow_clients` exercises a full realtime flow with a capacity-one client queue, asserting that the WARN/Error notice survives backpressure alongside the broadcast copy.【F:core/src/session/mod.rs†L210-L277】【dacffe†L5-L25】

2. **Local engine silently degrades to fallback stub** — *Status: Completed*
   - Remediation: `EngineOrchestrator::new` now propagates Whisper initialisation failures instead of silently swapping in `FallbackSpeechEngine`, only permitting a stub when `WHISPER_ALLOW_FALLBACK` is explicitly set so misconfigurations surface immediately in production builds.【F:core/src/orchestrator/mod.rs†L39-L48】【F:core/src/orchestrator/mod.rs†L230-L255】
   - Validation: `fails_when_whisper_env_missing_without_fallback` ensures missing Whisper state rejects startup, while `allows_fallback_when_explicitly_opted_in` verifies the guarded escape hatch for tests and tooling.【F:core/src/orchestrator/mod.rs†L1181-L1204】【dacffe†L5-L25】

## Status
- Gap 35 (Dropped realtime notices for slow session clients) — *Completed*.
- Gap 36 (Local engine silently degrades to fallback stub) — *Completed*.
