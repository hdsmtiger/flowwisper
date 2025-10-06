# UC2.1 Review (Round 7)

## Context
This round reassesses the realtime transcription implementation against the Sprint 2 UC2.1 acceptance criteria and the architectural mandate for sustained local-first streaming. The review focuses on the changes landed after round six.

## Findings

1. **Cloud updates resume without waiting for Whisper after the first local diff** — *Status: Completed*
   - Remediation: Replaced the global `local_update_flag` with a `LocalProgress` tracker that records the last Whisper frame, exposes degradation state, and gates every cloud task on per-frame deadlines so local output always leads unless the session is explicitly degraded.【F:core/src/orchestrator/mod.rs†L86-L189】【F:core/src/orchestrator/mod.rs†L324-L412】【F:core/src/orchestrator/mod.rs†L520-L606】
   - Validation: `cargo test --manifest-path core/Cargo.toml` (see `cloud_waits_for_local_each_frame`) confirms the cloud branch does not pre-empt subsequent Whisper increments.【8924ea†L1-L4】【F:core/src/orchestrator/mod.rs†L1099-L1155】

2. **Watchdog only covers the very first update** — *Status: Completed*
   - Remediation: Introduced a rolling cadence monitor that re-arms after every interval, issues incremental latency notices, and records per-frame latencies based on the frame start instant so SLA breaches remain visible beyond the first diff.【F:core/src/orchestrator/mod.rs†L90-L189】【F:core/src/orchestrator/mod.rs†L561-L606】
   - Validation: `cargo test --manifest-path core/Cargo.toml` exercises the renewed watchdog through the existing late-local test, which now observes cadence notices emitted after sustained stalls.【8924ea†L1-L4】【F:core/src/orchestrator/mod.rs†L1009-L1098】

## Next Steps
The round-seven gaps have been remediated and validated; keep the regression tests in CI to guard the local-first and cadence guarantees as UC2.1 evolves.
