# UC2.1 Review (Round 21)

## Context
Re-evaluated the realtime transcription pipeline after the round-20 fixes to confirm UC2.1 still satisfies the Sprint 2 cadence requirements and architecture guarantees around audio delivery, fallback behaviour, and UI feedback loops.【F:docs/sprint/sprint2.md†L8-L11】【F:docs/architecture.md†L92-L107】

## Findings

1. **Pending PCM tail is flushed after the subscriber is dropped** — *Status: Completed*
   - Evidence: The session manager only calls `AudioPipeline::flush_pending` after the realtime frame channel closes, at which point `collect_subscribers` prunes the closed sender so the padded tail never reaches Whisper.【F:core/src/session/mod.rs†L61-L88】【F:core/src/audio/mod.rs†L265-L327】
   - Impact: UC2.1 explicitly requires every 100–200 ms slice, including the trailing <100 ms remainder, to reach the decoder; dropping the tail violates the streaming contract and risks losing the user’s last syllables.【F:docs/sprint/sprint2.md†L8-L11】【F:docs/architecture.md†L96-L103】
   - Remediation: `SessionManager` now flushes pending PCM while the subscriber remains registered and drains the resulting frames before teardown so the padded tail reaches Whisper before the channel closes.【F:core/src/session/mod.rs†L61-L114】【F:core/src/audio/mod.rs†L265-L327】

2. **Slow per-session clients can block the broadcast bus** — *Status: Completed*
   - Evidence: The updates fan-out task awaits `client_tx.send` on the same loop that drains the orchestrator channel and forwards to the broadcast bus; when the client queue fills, the loop stalls and backpressures the orchestrator, preventing the state manager from emitting mandated 200 ms cadence updates and fallback notices.【F:core/src/session/mod.rs†L90-L104】【F:docs/architecture.md†L92-L107】
   - Impact: UC2.1’s UX contract requires continuous state feedback within 400 ms/200 ms windows; letting a single slow subscriber freeze the broadcast path breaks the HUD/status update guarantees described in the architecture doc.【F:docs/sprint/sprint2.md†L8-L11】【F:docs/architecture.md†L92-L107】
   - Remediation: The updates relay keeps broadcast delivery synchronous but switches per-session sends to non-blocking `try_send`, dropping only the affected client backlog so slow consumers cannot stall the orchestrator or suppress fallback notices.【F:core/src/session/mod.rs†L99-L120】

## Status
- Gap 33 (Pending PCM tail is flushed after the subscriber is dropped) — *Completed*.
- Gap 34 (Slow per-session clients can block the broadcast bus) — *Completed*.
