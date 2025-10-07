# Dual-View Transcript Onboarding

## Purpose
This guide helps engineers and QA ramp onto the UC2.2 dual-view transcript feature. It summarizes how raw and polished sentences flow through the stack, the user-facing behaviors we must exercise, and the current limitations to communicate during enablement or pilot rollouts.

## Runtime Data Flow
1. **Core orchestrator** chunks Whisper deltas into sentences, flushing raw text every 200 ms or once punctuation lands, and schedules polishing with a 2.5 s SLA via `RealtimeSessionConfig` defaults. 【F:core/src/orchestrator/mod.rs†L327-L349】
2. **Sentence polishing** is fulfilled by the `SentencePolisher` trait; the default `LightweightSentencePolisher` applies whitespace cleanup, filler-word trimming, pronoun capitalization, and trailing punctuation to keep the polished column lightly edited while staying conversational. 【F:core/src/orchestrator/mod.rs†L35-L120】
3. **Session manager** relays transcript updates and notices to the desktop bridge, trimming history to the most recent 120 events before emitting `session://transcript`. 【F:apps/desktop/src-tauri/src/session.rs†L8-L33】【F:apps/desktop/src-tauri/src/session.rs†L233-L282】
4. **Tauri layer** exposes transcript commands such as `session_transcript_log` for hydration and `session_transcript_apply_selection` for revert acknowledgements so the React shell can replay history and respond to user selections. 【F:apps/desktop/src-tauri/src/main.rs†L187-L217】【F:apps/desktop/src-tauri/src/main.rs†L1017-L1054】
5. **React hook (`useDualViewTranscript`)** fetches the backfilled log, subscribes to `session://transcript`, and manages selection, pending, notice, and announcement state for the Dual View panel. 【F:apps/desktop/src/features/transcription/hooks/useDualViewTranscript.ts†L1-L210】【F:apps/desktop/src/features/transcription/hooks/useDualViewTranscript.ts†L620-L747】
6. **DualViewPanel** renders synchronized raw/polished columns, selection toolbars, and HUD banners using the hook’s derived state and accessibility metadata. 【F:apps/desktop/src/features/transcription/DualViewPanel.tsx†L426-L535】

## Desktop Interaction Checklist
- **Selection guardrails:** Users can toggle up to five sentences at once; the hook announces when the cap is exceeded and blocks additional selections. 【F:apps/desktop/src/features/transcription/hooks/useDualViewTranscript.ts†L68-L110】【F:apps/desktop/src/features/transcription/hooks/useDualViewTranscript.ts†L522-L600】
- **Pending + acknowledgement flow:** Calling `markPendingSelection` raises polite announcements and defers state changes until the core responds with a `Selection` payload, at which point the UI clears pending badges and confirms success. 【F:apps/desktop/src/features/transcription/hooks/useDualViewTranscript.ts†L522-L580】【F:apps/desktop/src/features/transcription/hooks/useDualViewTranscript.ts†L304-L366】
- **Notices & banners:** `Notice` events map to HUD banners with tone-specific styles (info/warn/error) and assertive screen-reader messages for errors. 【F:apps/desktop/src/features/transcription/hooks/useDualViewTranscript.ts†L256-L303】【F:apps/desktop/src/features/transcription/DualViewPanel.tsx†L426-L471】
- **Accessibility:** Sentences receive generated aria labels describing active variants, latency status, and pending transitions, while keyboard helpers drive focus traversal between cards. 【F:apps/desktop/src/features/transcription/hooks/useDualViewTranscript.ts†L111-L210】【F:apps/desktop/src/features/transcription/hooks/useDualViewTranscript.ts†L600-L747】

## Observability & QA Tips
- **Telemetry files:** Dual-view latency and revert events are written to JSON lines at `logs/telemetry/dual-view.json*` (override with `FLOWWISPER_TELEMETRY_DIR`). Use this when validating SLA compliance or revert selections during manual testing. 【F:core/src/telemetry/mod.rs†L12-L58】
- **Smoke validation:** After running a session, confirm that both raw and polished variants log matching `sentence_id` values and that `within_sla` flips to `false` when deadlines are breached to ensure the end-to-end path is intact. 【F:core/src/telemetry/events.rs†L1-L120】【F:core/src/orchestrator/mod.rs†L1118-L1206】
- **Headless environments:** The hook skips hydration when the Tauri bridge is absent (e.g., storybook or pure web renders); integration tests must continue mocking `listen`/`invoke` to simulate session events. 【F:apps/desktop/src/features/transcription/hooks/useDualViewTranscript.ts†L84-L110】【F:apps/desktop/src/features/transcription/hooks/useDualViewTranscript.ts†L620-L747】

## Known Limitations to Communicate
- **Lightweight polishing heuristics:** The default polisher only performs light-touch cleanup (filler removal, capitalization, punctuation). Complex rephrasing still requires future LLM integration, so QA should expect subtle changes rather than fully rewritten prose. 【F:core/src/orchestrator/mod.rs†L35-L120】
- **History truncation:** Transcript history is capped at 120 events, so long-running sessions may evict earlier sentences from hydration. QA should cross-check live streaming rather than relying solely on the log dump. 【F:apps/desktop/src-tauri/src/session.rs†L8-L33】【F:apps/desktop/src-tauri/src/session.rs†L233-L268】
- **Cloud fallback messaging:** Notices currently reflect latency breaches or polishing failures but do not yet differentiate specific engine causes beyond “cloud fallback” wording; expect generic warning copy during outages. 【F:apps/desktop/src/features/transcription/hooks/useDualViewTranscript.ts†L228-L304】
