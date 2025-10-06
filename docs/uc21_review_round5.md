# UC2.1 Review (Round 5)

## Context
This round revalidates the Sprint 2 UC2.1 implementation against the PRD, architecture design, and sprint goals. The focus is on the realtime transcription path shipped in commit `fix: harden uc2.1 realtime fallbacks`.

## Outstanding Gaps

1. **Local-first feedback ordering is still not enforced**
   - UC2.1 and the architecture both require the Whisper local engine to surface the first characters within 400 ms so the user’s initial feedback is privacy-preserving and network-independent. 【F:docs/sprint/sprint2.md†L8-L11】【F:docs/architecture.md†L53-L69】
   - `RealtimeWorker` currently allows the cloud branch to emit the very first update (shared `first_update_flag`), and the regression test `emits_deadline_notice_when_local_is_late` codifies this cloud-first behaviour even when `prefer_cloud = false`. 【F:core/src/orchestrator/mod.rs†L339-L521】【F:core/src/orchestrator/mod.rs†L818-L855】
   - **Recommendation:** Gate cloud emissions until the local decoder has produced its first chunk (or at least mark cloud outputs as non-first) when running in local-first mode, ensuring the session’s first feedback truly originates from Whisper within the deadline.

2. **Whisper streaming still performs full-window decodes per frame**
   - UC2.1 expects 200 ms incremental cadence over multi-minute sessions. The architecture explicitly calls for incremental decoding / checkpoints so long recordings stay stable. 【F:docs/sprint/sprint2.md†L8-L11】【F:docs/architecture.md†L70-L106】
   - `WhisperLocalEngine::transcribe` rebuilds an up-to-8 s PCM window and calls `state.full` on every 100–200 ms frame. This repeated full decode grows linearly in cost with window size and makes sustained 200 ms cadences unlikely once the buffer fills, especially on CPU-only devices. 【F:core/src/orchestrator/mod.rs†L600-L711】
   - **Recommendation:** Switch to Whisper’s streaming partial APIs (e.g., `decode`/`encode` loops) or maintain incremental token state so each frame only decodes the fresh tail instead of replaying the entire window.

3. **Backpressure handling can drop audio frames silently**
   - The UC2.1 acceptance criteria assume every 100–200 ms frame reaches the decoder to preserve timing. 【F:docs/sprint/sprint2.md†L8-L11】
   - `RealtimeSessionHandle::push_frame` uses `try_send`, so any queue saturation silently drops frames (only a warning is logged). Since the audio pipeline pushes frames continuously, losing a frame would stretch the perceived cadence beyond 200 ms and undermine latency guarantees. 【F:core/src/orchestrator/mod.rs†L215-L238】
- **Recommendation:** Backpressure the producer (await `send`) or implement buffering with bounded retry so frames are not discarded without the session manager knowing.

## Next Steps
- Address the gaps above and update this tracker once fixes are validated with targeted latency / ordering tests.

## Remediation Updates

- ✅ Local-first gating now waits on the Whisper deadline notifier before allowing cloud updates to publish in `prefer_cloud = false` mode, ensuring the 400 ms watchdog can only be satisfied by the local decoder unless the deadline expires. The realtime worker emits a notify on the first local diff so cloud-first delivery cannot preempt Whisper in local-first sessions. Validation: `cargo test --manifest-path core/Cargo.toml`.
- ✅ Whisper streaming keeps a rolling window with 400 ms lookback and avoids replaying the full session on every frame by decoding only the most recent tail with offset tracking, preserving incremental cadence across long recordings. Validation: `cargo test --manifest-path core/Cargo.toml`.
- ✅ Audio frame submission now awaits the channel send and surfaces errors to the caller, eliminating silent drops when the queue is saturated and aligning with UC2.1’s 100–200 ms pacing guarantee. Validation: `cargo test --manifest-path core/Cargo.toml`.

