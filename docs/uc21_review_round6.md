# UC2.1 Review (Round 6)

## Context
This round validates the "local-first realtime transcription" behaviour introduced after round five. The assessment compares the current implementation with the Sprint 2 UC2.1 acceptance criteria, the PRD, and the architecture document.

## Findings

1. **Cloud fallback still claims the first transcript slot when Whisper misses the 400 ms target** — *Status: Completed*
   - UC2.1 requires Whisper to surface the first raw characters within 400 ms so the first user-visible feedback is produced locally. 【docs/sprint/sprint2.md†L8-L11】【docs/architecture.md†L53-L69】
   - `spawn_cloud_task` flips the shared `first_update_flag` even in local-first mode once the 400 ms timeout elapses, and the regression test `emits_deadline_notice_when_local_is_late` asserts that the cloud diff is delivered with `is_first = true`. This codifies cloud-first behaviour after a deadline miss instead of flagging the session as degraded. 【core/src/orchestrator/mod.rs†L502-L520】【core/src/orchestrator/mod.rs†L849-L915】
   - **Remediation:** Added a dedicated `first_local_update_flag`, gated the `is_first` bit to Whisper-sourced diffs, and introduced an explicit degradation notice for late locals so fallback transcripts remain marked as non-first.
   - **Validation:** `cargo test --manifest-path core/Cargo.toml` (see `emits_deadline_notice_when_local_is_late`).

2. **Local engine failures only log and never surface a fallback notice** — *Status: Completed*
   - UC2.1’s acceptance criteria call for logging and presenting a fallback prompt whenever realtime decoding encounters an anomaly. 【docs/sprint/sprint2.md†L8-L11】
   - In the local branch, transcription errors are only logged; no `SessionNotice` is pushed to the session bus, leaving the UI unaware that the local path failed and that it should highlight the fallback. 【core/src/orchestrator/mod.rs†L445-L483】
   - **Remediation:** Emitted an error-level `SessionNotice` from the Whisper path on failures and unblocked the cloud worker via notify so the UI immediately reflects the degraded local state.
   - **Validation:** `cargo test --manifest-path core/Cargo.toml` (see `emits_notice_when_local_engine_fails`).

3. **Audio fan-out still drops frames under subscriber lag** — *Status: Completed*
   - UC2.1 assumes every 100–200 ms PCM frame reaches the decoder to preserve the incremental cadence. 【docs/sprint/sprint2.md†L8-L11】
   - `SessionManager` handles `broadcast::RecvError::Lagged` by logging a warning and skipping the missed audio. This breaks the cadence guarantees and reintroduces the "dropped frame" issue the gap tracker flagged earlier. 【core/src/session/mod.rs†L61-L109】
   - **Remediation:** Replaced the lossy broadcast bridge with per-subscriber bounded `mpsc` queues guarded by a mutex so realtime sessions apply backpressure instead of dropping frames.
   - **Validation:** `cargo test --manifest-path core/Cargo.toml` (see `session::tests::routes_audio_frames_and_broadcasts_updates`).

## Next Steps
All round-six recommendations are now implemented and covered by regression tests; continue monitoring UC2.1 runs for latency regressions and revisit if additional scenarios emerge.
